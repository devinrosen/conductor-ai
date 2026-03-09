# Claude Agent Integration

Integration of Claude CLI agent execution into Conductor's TUI for Phase 4 (AI orchestration hooks).

## Architecture

Conductor shells out to `claude -p` via `std::process::Command`, consistent with the existing subprocess pattern (`git`, `gh`, `acli`). Agents run in tmux windows — the TUI orchestrates, tmux executes.

```
┌───────────────┐     ┌──────────────────────────┐
│  TUI          │     │  tmux session            │
│               │     │                          │
│  Worktree A   │────►│  window: claude agent A  │
│  [running]    │     │  window: claude agent B  │
│               │     │  window: claude agent C  │
│  Worktree B   │     │                          │
│  [done]       │     └──────────────────────────┘
│               │
│  Worktree C   │     Agent status comes from DB,
│  [running]    │     not from streaming output.
└───────────────┘
```

### Event Loop (Priority Channels)

The TUI uses separate channels for input vs background events. Input is always drained first, so keystrokes are never blocked by DB polls or other background work.

```
┌──────────────────────────────────────────────────┐
│  Main Loop                                       │
│                                                  │
│  1. Block on wake_rx (any channel has data)      │
│  2. Drain input_rx (all keys)    ◄── HIGH PRIO   │
│  3. Drain bg_rx (backgrounds)    ◄── LOW PRIO    │
│  4. Draw if dirty                                │
│                                                  │
│  input_rx ◄── Input thread (10ms poll)           │
│  bg_rx    ◄── DB poller, tick timer              │
└──────────────────────────────────────────────────┘
```

### Agent Lifecycle

1. User presses `r` on a worktree → input modal for prompt (pre-filled with ticket URL if linked)
2. On submit: `AgentManager::create_run()` persists a record to `agent_runs` table with `tmux_window` set to the worktree slug
3. TUI spawns `tmux new-window -n <worktree-slug> -- conductor agent run --run-id <id> --worktree-path <path> --prompt <prompt>`
4. `conductor agent run` executes `claude -p` in the tmux window — stderr inherited (visible output), stdout piped (JSON result)
5. On completion: parses `ClaudeJsonResult` from stdout, updates DB with status, session_id, cost, turns, duration
6. TUI polls `agent_runs` table every 5s via background DB poller — zero streaming events through the event loop
7. User presses `r` again → auto-resumes via `--resume <session_id>` from DB
8. User presses `a` → attaches to the agent's tmux window
9. User presses `x` → kills the tmux window, marks run as cancelled in DB

### Database Schema

```sql
CREATE TABLE agent_runs (
    id                TEXT PRIMARY KEY,
    worktree_id       TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    claude_session_id TEXT,
    prompt            TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','completed','failed','cancelled')),
    result_text       TEXT,
    cost_usd          REAL,
    num_turns         INTEGER,
    duration_ms       INTEGER,
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    tmux_window       TEXT
);
```

Migrations: `003_agent_runs.sql` (table), `004_agent_tmux.sql` (adds `tmux_window` column).

### CLI Subcommand

```
conductor agent run
  --run-id <ID>            # agent_runs row ID
  --worktree-path <PATH>   # directory to run claude in
  --prompt <TEXT>           # prompt for claude
  --resume <SESSION-ID>    # optional: resume previous session
```

This is an internal command spawned by the TUI inside tmux. It:
1. Verifies the run exists in DB
2. Runs `claude -p <prompt> --output-format json --verbose --permission-mode acceptEdits`
3. Pipes stdout (JSON result), inherits stderr (visible in tmux terminal)
4. Parses `ClaudeJsonResult` and updates the DB record

### JSON Result Format

The `claude -p --output-format json` output is a single JSON object:

```json
{
  "session_id": "sess-abc",
  "result": "Final output text",
  "cost_usd": 0.05,
  "num_turns": 3,
  "duration_ms": 15000,
  "is_error": false
}
```

### TUI Key Bindings (Worktree Detail)

| Key | Action | Condition |
|-----|--------|-----------|
| `r` | Launch/resume agent | Active worktree |
| `a` | Attach to tmux window | Agent is running |
| `x` | Stop running agent | Agent is running |

### Agent Status Display

The worktree detail view shows agent status from the latest `agent_runs` row:

- **Running**: `[running] — press a to attach, x to stop`
- **Completed**: `[completed] $0.0234, 3 turns, 12.5s  session: <sid-abbr>`
- **Failed**: `[failed] <error-truncated>`
- **Cancelled**: `[cancelled]`

### Performance

- **Zero TUI load per agent**: Agents run in tmux, not through the event loop
- **Priority input channel**: Key events always processed before background events
- **Dirty-flag rendering**: Only redraws when state actually changed
- **DataRefreshed no-redraw**: Background DB polls update state silently without triggering redraws
- **10ms input polling**: Crossterm input reader polls at 10ms (decoupled from 200ms tick rate)
- **KeyEventKind::Press filter**: Only processes key press events, ignores release/repeat
- **Batch event drain**: Drains all queued events before each redraw
- **Binary path resolution**: Looks for `conductor` next to the TUI executable, falls back to PATH

### Concurrency

Each worktree can have its own agent running in a separate tmux window. The `latest_runs_by_worktree()` query efficiently returns the latest run per worktree in a single SQL query (used by the DB poller every 5s). Agents get a real terminal in tmux — full interactive capability, scrollback preserved.

---

## Research: Claude Agent SDK vs. Subprocess vs. tmux

*Researched 2026-03-09*

### What is the Claude Agent SDK?

The Claude Agent SDK (Python/TypeScript) wraps the `claude` CLI's agent loop and exposes it programmatically. It handles the full tool execution loop automatically — Claude evaluates, calls tools, gets results, and repeats until done. Key features:

- **Structured lifecycle**: `ResultMessage.subtype` gives clean `success` / `error_max_turns` / `error_during_execution` states
- **Session management**: sessions persist as `.jsonl` files; resumption via `resume=session_id`
- **Cost + usage tracking**: `total_cost_usd`, `num_turns`, token counts per run
- **Permission hooks**: `canUseTool` callback surfaces approval requests programmatically
- **Streaming output**: yields `AssistantMessage`, `UserMessage`, `ResultMessage` as the loop runs

### Why the SDK doesn't apply directly to Conductor

**The SDK is Python/TypeScript only.** Conductor is Rust. Options:

1. **Wrap in a Python/TS subprocess** — adds a runtime dependency, another process layer, and the complexity of managing that subprocess from Rust.
2. **Call `claude` CLI directly** — the SDK is essentially a wrapper around this anyway. Use `--output-format stream-json` for structured output.
3. **Keep tmux** — users can watch/interact with the agent in real-time; human-in-the-loop is a feature.

The SDK is designed for "embed Claude in your app and build the UI yourself" use cases. Conductor's current UX *is* the tmux terminal — that interactive terminal window has genuine value.

### Billing: SDK vs. CLI

| Mode | Auth | Billing |
|------|------|---------|
| `claude` CLI | Claude Code subscription | Included in subscription |
| Claude Agent SDK | `ANTHROPIC_API_KEY` | Usage-based, separate billing |
| Direct Anthropic API | `ANTHROPIC_API_KEY` | Usage-based, separate billing |

Switching to the SDK would change the billing model for users.

### Option: Headless subprocess (`--output-format stream-json`)

Instead of tmux, conductor could manage the `claude` process directly:

```
┌───────────────┐     ┌────────────────────────────────┐
│  AgentManager │     │  claude subprocess (per run)   │
│               │     │                                │
│  spawn()      │────►│  claude -p "..." \             │
│  read stdout  │◄────│    --output-format stream-json │
│  update DB    │     │    --permission-mode acceptEdits│
└───────────────┘     └────────────────────────────────┘
```

**Gains:**
- Eliminates tmux dependency and orphan reaper logic
- Real-time structured output → live status in TUI without 5s polling delay
- Cleaner process lifecycle — death of subprocess is immediately detectable
- Single `conductor-tui` or `conductor-web` process owns everything

**Loses:**
- No interactive terminal for the user to attach to (no `press a to attach`)
- No scrollback / visible execution trace in a tmux pane
- Agent runs tied to conductor process lifetime (background daemon becomes necessary for resilience)

### When each approach makes sense

| Concern | tmux | Headless subprocess |
|---------|------|---------------------|
| User watches agent work | Best | Not possible |
| Human-in-the-loop prompts | Easy (attach to pane) | Needs separate UI |
| Process resilience if TUI crashes | Good (tmux outlives TUI) | Run dies with TUI |
| Structured output / live status | Polling only | Native streaming |
| No extra runtime dependency | Yes | Yes (same) |
| Orphan reaper complexity | Required | Not needed |
| v2 daemon extraction | Fits naturally | Also fits naturally |

### Process Resilience: headless subprocess vs. tmux

A common concern with the headless approach is whether the agent survives a TUI crash or restart.

**Default `Command::spawn()` behavior (Unix):** child processes survive parent exit — they get reparented to PID 1. So `conductor agent run` (and `claude` underneath it) would keep running if the TUI exits cleanly.

**The edge case that kills it:** if the user closes the terminal window while the TUI is running, the OS sends `SIGHUP` to the entire process group. A directly-spawned child inherits the TUI's process group and controlling terminal, so it receives SIGHUP and dies — taking the agent with it.

tmux avoids this because `conductor agent run` runs inside a tmux session with its own process group and no controlling terminal tied to the user's window.

**The fix:** spawn the child in its own process group:

```rust
Command::new("conductor")
    .args(["agent", "run", ...])
    .process_group(0)  // new process group, detached from TUI's group
    .spawn()
```

With `process_group(0)`, the child won't receive SIGHUP when the terminal closes — same resilience guarantee as tmux, without the extra indirection.

**PID tracking:** after a TUI restart, you need to reconnect to the running process. With tmux that's a window name lookup; with headless it requires storing the PID in the DB (or a pidfile) and checking liveness via `kill -0 <pid>`. The orphan reaper would replace tmux window checks with PID checks.

### Recommendation

The tmux approach remains the right default for the **interactive TUI use case**. The headless subprocess approach becomes compelling when:

- Building a daemon (v2) where the daemon owns agent processes and TUI is a thin client
- Running agents from the web UI where there's no terminal to attach to
- Wanting live streaming output in the TUI panel (vs. 5s polling)

A hybrid is possible: use `stream-json` subprocess for the web backend (which has no tmux), keep tmux for the TUI where attach-to-agent is a first-class interaction.

---

## Future: v2 Daemon Extraction

See `docs/VISION.md` section "v2: Daemon Extraction" for the full analysis. The daemon becomes the single owner of agent processes, DB writes, and state. The TUI becomes a thin display client connected via IPC.

---

## Reference: Claude CLI Flags

```bash
# Non-interactive execution
claude -p "Your prompt here"

# Output formats
claude -p "task" --output-format json         # Single JSON result (used by conductor)
claude -p "task" --output-format stream-json   # Real-time JSON stream (requires --verbose)

# Session management
claude -p "Continue" --resume <session-id>
claude -p "Fork" --fork-session <session-id>

# Permission modes
claude -p "task" --permission-mode plan           # Read-only analysis
claude -p "task" --permission-mode acceptEdits     # Allow file writes (used by conductor)
claude -p "task" --permission-mode bypassPermissions  # Allow everything (sandboxed only)

# Tool control
claude -p "Review" --allowedTools "Read,Glob,Grep"

# Custom system prompts
claude -p "task" --append-system-prompt "Custom instructions"
```

## Billing Note

The `claude` CLI uses the existing Claude Code subscription auth. No separate API key needed. The Claude Agent SDK (Python/TypeScript) and direct Anthropic API calls require a separate `ANTHROPIC_API_KEY` from [console.anthropic.com](https://console.anthropic.com) with usage-based billing.
