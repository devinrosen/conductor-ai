# RFC 007: Multi-Runtime Agent Support

**Status:** Draft
**Date:** 2026-03-18
**Author:** Devin

---

## Problem

Conductor workflows currently execute all agents via Claude Code in tmux sessions. This couples the entire workflow engine to a single AI provider. Users want to:

- Use **Gemini** for research-heavy steps (web search, large-context summarization)
- Use **OpenAI** models for specific tasks where they excel
- Run **custom scripts** or alternative CLI agents (Aider, Codex CLI, ADK) as workflow steps
- Mix runtimes within a single workflow (e.g., Gemini researches, Claude implements)

## Research: How Other Ecosystems Handle Model Selection

All three major ecosystems use **flat string model IDs** with a resolution chain:

| Framework | Agent-Level | Per-Run Override | Default |
|-----------|------------|-----------------|---------|
| **Claude Code** | Frontmatter `model:` | CLI `--model` | Runtime default |
| **Google ADK** | `Agent(model='gemini-2.5-flash')` or YAML `model:` | Per sub-agent | `gemini-2.5-flash` |
| **OpenAI Agents SDK** | `Agent(model="gpt-4.1")` | `RunConfig(model=)` | `gpt-4.1` |
| **Gemini CLI** | `settings.json` | `--model` flag, `GEMINI_MODEL` env | Auto-route |
| **Codex CLI** | `config.toml` | `--model` flag, profiles | `gpt-5-codex` |

Key takeaway: the `model` field is always an **opaque string** interpreted by the runtime. Conductor can pass it through without knowing valid values.

### Execution Model Differences

| Runtime | Execution Model | Needs Tmux? | Tool Use? |
|---------|----------------|-------------|-----------|
| Claude Code | CLI agent with file/bash/git tools | Yes | Full agentic |
| Gemini CLI | CLI agent, similar to Claude Code | Yes (same pattern) | Full agentic |
| Codex CLI | CLI agent, similar to Claude Code | Yes (same pattern) | Full agentic |
| Gemini API | Stateless HTTP request/response | No | Prompt → text |
| OpenAI API | Stateless HTTP request/response | No | Prompt → text |

This suggests two execution strategies:
1. **CLI-based** (tmux spawn + DB poll) — Claude Code, Gemini CLI, Codex CLI
2. **API-based** (direct HTTP call, synchronous) — Gemini API, OpenAI API

---

## Proposed Design

### 1. Agent Frontmatter: Add `runtime` Field

```yaml
# .conductor/agents/research.md
---
role: reviewer
runtime: gemini
model: gemini-2.5-flash
---

Research the following topic: {{topic}}
```

- `runtime` defaults to `"claude"` if omitted — fully backwards compatible
- `model` remains an opaque string, interpreted by the runtime

Changes to `AgentFrontmatter` and `AgentDef`:

```rust
struct AgentFrontmatter {
    role: String,
    can_commit: bool,
    model: Option<String>,
    runtime: String,       // new — defaults to "claude"
}

pub struct AgentDef {
    pub name: String,
    pub role: AgentRole,
    pub can_commit: bool,
    pub model: Option<String>,
    pub runtime: String,   // new
    pub prompt: String,
}
```

### 2. Config: Runtime Credentials

```toml
# ~/.conductor/config.toml

[runtimes.gemini]
api_key_env = "GEMINI_API_KEY"
default_model = "gemini-2.5-flash"

[runtimes.openai]
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4.1"

[runtimes.script]
command = "python3 my_agent.py"
```

```rust
pub struct Config {
    // ... existing fields ...
    pub runtimes: HashMap<String, RuntimeConfig>,
}

pub struct RuntimeConfig {
    /// Env var name holding the API key (not the key itself — no secrets in config)
    pub api_key_env: Option<String>,
    /// Default model for this runtime (overridden by agent/step)
    pub default_model: Option<String>,
    /// For "script" runtime: the command to execute
    pub command: Option<String>,
}
```

### 3. Runtime Trait

```rust
// conductor-core/src/runtime/mod.rs

pub struct RuntimeResult {
    pub status: AgentRunStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

pub struct RuntimeRequest {
    pub run_id: String,
    pub prompt: String,
    pub model: Option<String>,
    pub working_dir: String,
    pub agent_def: AgentDef,
}

pub trait AgentRuntime {
    /// Execute the agent. CLI runtimes spawn a tmux window.
    /// API runtimes make an HTTP call (blocking).
    fn spawn(
        &self,
        request: &RuntimeRequest,
        window_name: &str,
    ) -> Result<(), String>;

    /// Poll for completion. CLI runtimes poll the DB.
    /// API runtimes complete during spawn() — poll just reads from DB.
    fn poll(
        &self,
        conn: &Connection,
        run_id: &str,
        poll_interval: Duration,
        timeout: Duration,
        shutdown: Option<&Arc<AtomicBool>>,
    ) -> Result<AgentRun, PollError>;

    /// Check if the agent process is still alive.
    fn is_alive(&self, run: &AgentRun) -> bool;

    /// Cancel a running agent.
    fn cancel(&self, run: &AgentRun) -> Result<(), String>;
}
```

### 4. Runtime Implementations

**ClaudeRuntime** — extracts existing `agent_runtime.rs` logic unchanged:
- `spawn()` → `spawn_child_tmux()`
- `poll()` → `poll_child_completion()`
- `is_alive()` → checks `list_live_tmux_windows()`
- `cancel()` → kills tmux window

**GeminiRuntime** — API-based, no tmux:
- `spawn()` → synchronous HTTP POST to `generativelanguage.googleapis.com`, writes result to DB
- `poll()` → reads completed run from DB (already finished in spawn)
- `is_alive()` → always `false`
- `cancel()` → no-op

**OpenAIRuntime** — same pattern as Gemini, different API endpoint.

**ScriptRuntime** — escape hatch for arbitrary commands:
- `spawn()` → runs command via `Command::new("sh")`, passes prompt via env var `CONDUCTOR_PROMPT`
- Captures stdout as `result_text`

### 5. Runtime Resolution

```rust
fn resolve_runtime(name: &str, config: &Config) -> Result<Box<dyn AgentRuntime>> {
    let rt_config = config.runtimes.get(name);
    match name {
        "claude" => Ok(Box::new(ClaudeRuntime)),
        "gemini" => Ok(Box::new(GeminiRuntime::from_config(rt_config?))),
        "openai" => Ok(Box::new(OpenAIRuntime::from_config(rt_config?))),
        "script" => Ok(Box::new(ScriptRuntime::from_config(rt_config?))),
        _ => Err(format!("unknown runtime: {name}"))
    }
}
```

### 6. Workflow Executor Change

In `executors.rs::execute_call_with_schema`, replace the direct `spawn_child_tmux` / `poll_child_completion` calls with runtime dispatch:

```rust
let runtime = resolve_runtime(&agent_def.runtime, &state.config)?;
runtime.spawn(&request, &child_window)?;
let completed = runtime.poll(conn, &child_run.id, ...)?;
```

The workflow DSL itself does not change. Runtime is an agent-level concern:

```
call research:       # uses gemini (defined in research.md frontmatter)
call implement:      # uses claude (defined in implement.md frontmatter)
```

### 7. DB Migration

```sql
ALTER TABLE agent_runs ADD COLUMN runtime TEXT NOT NULL DEFAULT 'claude';
```

---

## Decisions Made

1. **Runtime is an agent-level concern**, not a workflow-step concern. The agent `.md` file declares which runtime it uses. This is consistent with how `role`, `model`, and `can_commit` work today.

2. **Model is an opaque string** passed through to the runtime. Conductor does not validate model IDs — each runtime knows its own valid values.

3. **API runtimes are synchronous.** `spawn()` does the full HTTP call and writes the result to DB. `poll()` just reads it back. This avoids introducing async into the workflow engine.

4. **Credentials via env var indirection.** Config stores the env var *name*, not the secret. No secrets in `config.toml`.

5. **`runtime` defaults to `"claude"`.** Fully backwards compatible — existing agent files work unchanged.

6. **`script` runtime as escape hatch.** Wraps any CLI tool (ADK, Codex, custom scripts) without needing native conductor support.

---

## Open Questions

1. **CLI-based runtimes (Gemini CLI, Codex CLI):** Should these use the same tmux spawn pattern as Claude, or should they be separate runtime types? If tmux-based, they could share a `CliRuntime` base that parameterizes the binary and args.

2. **Structured output for API runtimes:** Claude agents produce structured output via conductor's `--output-format json`. How should API runtimes produce structured output that integrates with the existing schema validation pipeline?

3. **Tool use for API runtimes:** Gemini and OpenAI APIs support function calling. Should conductor expose workflow context as tools to API-based agents, or keep them as simple prompt → text?

4. **Cost tracking:** Claude reports cost via its JSON output. Gemini/OpenAI APIs return token counts in response headers/bodies. Need to normalize cost reporting across runtimes.

5. **Agent capabilities validation:** `can_commit: true` makes no sense for an API-based agent. Should conductor validate that capability flags are compatible with the runtime?

6. **Async consideration:** API calls are fast but still block the workflow executor thread. For parallel blocks with many API calls, should we consider `ureq` (blocking) vs `reqwest` (async)? The current engine is synchronous, so `ureq` is the path of least resistance.

7. **Rate limiting:** API-based runtimes may hit rate limits. Should retry/backoff logic live in the runtime implementation or be handled by the existing retry mechanism in the workflow executor?

8. **Context window management:** Different runtimes have different context limits. Should conductor be aware of this, or leave it to the user to choose appropriate models?

---

## Implementation Order

1. Add `runtime` field to `AgentFrontmatter` / `AgentDef` (backwards-compatible default)
2. Add `RuntimeConfig` to `Config`
3. Define `AgentRuntime` trait
4. Extract existing tmux logic into `ClaudeRuntime` (pure refactor, no behavior change)
5. Add DB migration for `runtime` column
6. Wire runtime dispatch into `execute_call_with_schema`
7. Implement `GeminiRuntime` (API-based)
8. Implement `ScriptRuntime` (escape hatch)
9. Implement `OpenAIRuntime` (API-based)

Steps 1–6 can land as a single PR with no functional change (Claude-only, but via the trait). Steps 7–9 are independent and can land separately.
