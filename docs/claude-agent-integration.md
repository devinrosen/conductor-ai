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

## Future: v2 Daemon Extraction

See `docs/SPEC.md` section "v2: Daemon Extraction" for the full analysis. The daemon becomes the single owner of agent processes, DB writes, and state. The TUI becomes a thin display client connected via IPC.

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
