# RFC 007: Multi-Runtime Agent Support

**Status:** Draft
**Date:** 2026-03-28
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

# CLI-based runtimes (tmux spawn + exit polling, same pattern as Claude)
[runtimes.gemini]
type = "cli"
binary = "gemini"
args = ["-m", "{{model}}", "-p", "{{prompt}}", "--output-format", "json", "--approval-mode=yolo"]
default_model = "gemini-2.5-flash"
result_field = "response"
token_fields = "stats.models.*.tokens.total"

# API-based runtimes (synchronous HTTP, no tmux)
[runtimes.openai]
type = "api"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4.1"

# Script escape hatch
[runtimes.script]
type = "script"
command = "python3 my_agent.py"
```

```rust
pub struct Config {
    // ... existing fields ...
    pub runtimes: HashMap<String, RuntimeConfig>,
}

pub struct RuntimeConfig {
    /// "cli", "api", or "script"
    pub runtime_type: String,
    /// For "cli": binary to invoke (e.g. "gemini")
    pub binary: Option<String>,
    /// For "cli": arg template — {{prompt}} and {{model}} are substituted
    pub args: Option<Vec<String>>,
    /// For "cli": "arg" (default) or "stdin"
    pub prompt_via: Option<String>,
    /// For "cli"/"api": default model ID, overridden by agent frontmatter
    pub default_model: Option<String>,
    /// For "cli" with JSON output: dot-path to extract result text (e.g. "response")
    pub result_field: Option<String>,
    /// For "cli" with JSON output: dot-path to extract total tokens (optional)
    pub token_fields: Option<String>,
    /// For "api": env var name holding the API key (not the key itself)
    pub api_key_env: Option<String>,
    /// For "script": the shell command to execute
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

**CliRuntime** — generic tmux-based runner for any CLI agent (Gemini CLI, Codex CLI, etc.):
- `spawn()` → builds command from config template, spawns tmux window, polls for process exit
- `poll()` → same exit-based polling as `ClaudeRuntime`
- `is_alive()` → checks `list_live_tmux_windows()`
- `cancel()` → kills tmux window
- Result text extracted from stdout capture; token counts parsed from JSON output if `output_format = "json"` is configured

Config for a `CliRuntime` entry specifies the binary, arg template, output format, and field mappings:

```toml
[runtimes.gemini]
type = "cli"
binary = "gemini"
args = ["-m", "{{model}}", "-p", "{{prompt}}", "--output-format", "json", "--approval-mode=yolo"]
default_model = "gemini-2.5-flash"
result_field = "response"                        # jq-style path into JSON output
token_fields = "stats.models.*.tokens.total"     # optional, for cost tracking
```

`{{prompt}}` and `{{model}}` are the only substitution variables. If `prompt_via = "stdin"`, the prompt is written to the process's stdin instead of substituted into args.

**Gemini CLI invocation shape** (researched 2026-03-28):
- Binary: `gemini` (npm: `@google/gemini-cli`)
- Prompt flag: `-p "<prompt>"` — forces headless mode, process exits after response
- Model flag: `-m gemini-2.5-flash`
- Output: `--output-format json` → `{ "response": "...", "stats": { "models": { "gemini-2.5-flash": { "tokens": { "total": N } } } } }`
- Tool approval: `--approval-mode=yolo` to suppress interactive prompts
- Exit codes: `0` success, `1` error, `42` input error, `53` turn limit exceeded
- No dollar cost in output — token counts only

**OpenAIRuntime** — API-based, no tmux:
- `spawn()` → synchronous HTTP POST to OpenAI API, writes result to DB
- `poll()` → reads completed run from DB (already finished in spawn)
- `is_alive()` → always `false`
- `cancel()` → no-op

**ScriptRuntime** — escape hatch for arbitrary commands:
- `spawn()` → runs command via `Command::new("sh")`, passes prompt via env var `CONDUCTOR_PROMPT`
- Captures stdout as `result_text`

### 5. Runtime Resolution

```rust
fn resolve_runtime(name: &str, config: &Config) -> Result<Box<dyn AgentRuntime>> {
    let rt_config = config.runtimes.get(name);
    match name {
        "claude" => Ok(Box::new(ClaudeRuntime)),
        "script" => Ok(Box::new(ScriptRuntime::from_config(rt_config?))),
        "openai" => Ok(Box::new(OpenAIRuntime::from_config(rt_config?))),
        _ => {
            // Any runtime with type = "cli" in config resolves to CliRuntime
            let cfg = rt_config.ok_or_else(|| format!("unknown runtime: {name}"))?;
            if cfg.runtime_type == "cli" {
                Ok(Box::new(CliRuntime::from_config(cfg)))
            } else {
                Err(format!("unknown runtime type for '{name}'"))
            }
        }
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

### 7. Per-Repo Runtime Overrides

Runtime settings can be overridden on a per-repo basis via a `runtime_overrides` JSON column on the `repos` table. This is stored locally in each user's SQLite DB (`~/.conductor/conductor.db`) and does not affect other developers.

```json
{
  "claude": { "config_dir": "~/.claude-personal" },
  "gemini": { "config_dir": "~/.gemini-work" }
}
```

**Resolution chain** for any runtime setting:

```
per-repo SQLite runtime_overrides
  → global config.toml [runtimes.<name>]
    → compiled-in defaults
```

`RepoManager` exposes a typed accessor for reading these overrides:

```rust
impl RepoManager<'_> {
    /// Returns the resolved config_dir for a given runtime, checking
    /// per-repo overrides first, then global config, then the default.
    pub fn runtime_config_dir(
        &self,
        repo_id: &str,
        runtime: &str,
        global_config: &Config,
    ) -> Option<PathBuf>;
}
```

This design intentionally defers per-repo override *writes* (CLI/TUI surface) to the RFC 007 implementation, as the full runtime abstraction is needed to make the UX coherent.

### 8. DB Migrations

```sql
ALTER TABLE agent_runs ADD COLUMN runtime TEXT NOT NULL DEFAULT 'claude';
ALTER TABLE repos ADD COLUMN runtime_overrides TEXT;  -- JSON, nullable
```

---

## Decisions Made

1. **Runtime is an agent-level concern**, not a workflow-step concern. The agent `.md` file declares which runtime it uses. This is consistent with how `role`, `model`, and `can_commit` work today.

2. **Model is an opaque string** passed through to the runtime. Conductor does not validate model IDs — each runtime knows its own valid values.

3. **API runtimes are synchronous.** `spawn()` does the full HTTP call and writes the result to DB. `poll()` just reads it back. This avoids introducing async into the workflow engine.

4. **Credentials via env var indirection.** Config stores the env var *name*, not the secret. No secrets in `config.toml`.

5. **`runtime` defaults to `"claude"`.** Fully backwards compatible — existing agent files work unchanged.

6. **`script` runtime as escape hatch.** Wraps any CLI tool (ADK, Codex, custom scripts) without needing native conductor support for each one.

7. **CLI-based runtimes use a single generic `CliRuntime`**, not per-tool implementations. Any CLI agent that accepts a prompt (via flag or stdin) and exits on completion can be configured via `[runtimes.<name>]` with `type = "cli"` — no code changes required to add a new CLI tool. `ClaudeRuntime` stays separate because it has deep conductor integration (`--run-id`, resume, event parsing) that doesn't generalize.

8. **Per-repo runtime overrides live in SQLite**, not in a checked-in config file. A nullable `runtime_overrides TEXT` (JSON) column on `repos` stores a map of runtime name → override settings. Because `~/.conductor/conductor.db` is local to each user, this is safe for multi-developer repos — no shared state. The resolution chain is: per-repo SQLite → global `config.toml [runtimes.<name>]` → compiled-in defaults. `RepoManager` exposes a typed accessor so callers never parse JSON directly.

---

## Open Questions

1. **Structured output for API runtimes:** Claude agents produce structured output via conductor's `--output-format json` and schema validation. API runtimes return plain text. For now, skip schema validation for API-based agents — they return `result_text` only. Revisit if there's a real use case for structured output from an API runtime.

2. **Tool use for API runtimes:** Gemini and OpenAI APIs support function calling. Keep them as simple prompt → text for now. Conductor does not expose workflow context as tools to API runtimes.

3. **Cost tracking normalization:** Claude reports dollar cost. CLI runtimes (Gemini) report token counts only; no dollar cost. API runtimes return token counts in response bodies. Store token counts when available; leave `cost_usd` null for non-Claude runtimes until a cost estimation layer is warranted.

4. **Agent capabilities validation:** `can_commit: true` is meaningless for API-based agents (they can't modify files). Emit a warning at workflow parse time if `can_commit: true` is set on a non-CLI runtime agent; don't hard-error to keep config forgiving.

5. **Async consideration:** API calls block the workflow executor thread. `ureq` (blocking, no extra runtime) is the correct choice for now — consistent with the synchronous engine. Revisit if parallel blocks with many API calls cause visible latency.

6. **Rate limiting and retries:** Leave to the user for now. The existing workflow retry mechanism (`retry:` in DSL) covers transient failures. Per-runtime backoff can be added later if needed.

---

## Implementation Order

1. Add `runtime` field to `AgentFrontmatter` / `AgentDef` (backwards-compatible default `"claude"`)
2. Add `RuntimeConfig` to `Config` (with `type`, `binary`, `args`, `prompt_via`, `result_field`, etc.)
3. Define `AgentRuntime` trait (`spawn`, `poll`, `is_alive`, `cancel`)
4. Extract existing tmux logic into `ClaudeRuntime` (pure refactor, no behavior change)
5. Add DB migrations for `runtime` column on `agent_runs` and `runtime_overrides` column on `repos`
6. Add `RepoManager::runtime_config_dir()` typed accessor for per-repo override resolution
7. Wire runtime dispatch into `execute_call_with_schema`
8. Implement `CliRuntime` (generic tmux-based runner — covers Gemini CLI, Codex CLI, etc.)
9. Implement `ScriptRuntime` (escape hatch for arbitrary shell commands)
10. Implement `OpenAIRuntime` (API-based, optional — only if there's a concrete use case)

Steps 1–7 land as a single PR with no functional change (Claude-only, but via the trait). Steps 8–9 are independent and can land separately. Step 10 is deferred until there's a concrete use case.

---

## Out of Scope — Future Considerations

**Image generation from workflows:** Cloud-based image generation services (e.g. Nano Banana) are not CLI tools and don't fit the `CliRuntime` model — they're API-only with binary output rather than text. Generating images as a workflow step is a desirable future capability but requires a separate design: a dedicated step type (e.g. `generate-image:`), an output artifact model for binary files, and a storage layer for the results. Track separately when there's a concrete use case.
