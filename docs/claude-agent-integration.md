# Claude Agent Integration

Integration of Claude CLI agent execution into Conductor's TUI for Phase 4 (AI orchestration hooks).

## Current Implementation (v1)

Conductor shells out to `claude -p` via `std::process::Command`, consistent with the existing subprocess pattern (`git`, `gh`, `acli`).

### Architecture

```
┌─────────────────────────────────────────────────┐
│  TUI Main Thread (synchronous event loop)       │
│                                                 │
│  events.next() ──► update(action) ──► draw()    │
│       ▲                                         │
│       │ mpsc channel                            │
│  ┌────┴──────────────┐                          │
│  │ Input thread      │  10ms poll               │
│  │ Tick thread       │  200ms interval          │
│  │ DB poller thread  │  5s interval             │
│  │ Agent thread(s)   │  per-worktree            │
│  └───────────────────┘                          │
└─────────────────────────────────────────────────┘
```

### Agent Lifecycle

1. User presses `r` on a worktree → input modal for prompt (pre-filled with ticket URL if linked)
2. On submit: `AgentManager::create_run()` persists a record to `agent_runs` table
3. Background thread spawns `claude -p <prompt> --output-format stream-json --verbose --permission-mode acceptEdits`
4. Thread reads stdout line-by-line, parses JSON events, sends `AgentOutputLine` actions to main loop
5. On completion: `AgentCompleted` action stores session_id, cost, turns, duration in DB
6. User presses `r` again → resumes via `--resume <session_id>` from DB
7. User presses `f` → explicit follow-up on previous session
8. User presses `x` → sets `Arc<AtomicBool>` cancel flag, kills child process

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
    ended_at          TEXT
);
```

### CLI Flags Used

```bash
claude -p "prompt"              \
  --output-format stream-json   \  # Real-time JSON event stream
  --verbose                     \  # Required with stream-json
  --permission-mode acceptEdits \  # Allow file writes non-interactively
  --resume <session-id>           # Resume previous session (optional)
```

### Stream-JSON Event Types

The `claude -p --output-format stream-json` output emits one JSON object per line:

| Event Type | Key Fields | Usage |
|-----------|------------|-------|
| `system` | `session_id` | First event — extract session ID immediately |
| `assistant` | `message.content[].text` | Complete assistant message — display to user |
| `content_block_delta` | `delta.text` | Per-token streaming (currently ignored — too noisy) |
| `result` | `session_id`, `cost_usd`, `num_turns`, `duration_ms` | Final summary — persist to DB |

### TUI Key Bindings (Worktree Detail)

| Key | Action | Notes |
|-----|--------|-------|
| `r` | Launch/resume agent | Auto-resumes if previous session exists |
| `f` | Follow-up on session | Explicit `--resume` with new prompt |
| `x` | Stop running agent | Kills process, marks as cancelled |
| `j`/`k` | Scroll agent output | Only when agent panel is visible |

### Performance Optimizations

- **Dirty-flag rendering**: Only redraws when state actually changed
- **DataRefreshed no-redraw**: Background DB polls update state silently without triggering redraws
- **Output buffer cap**: Agent output capped at 2000 lines to bound layout cost
- **10ms input polling**: Crossterm input reader polls at 10ms (decoupled from 200ms tick rate)
- **KeyEventKind::Press filter**: Only processes key press events, ignores release/repeat
- **Batch event drain**: Drains all queued events before each redraw

### Known Limitations

1. **Single-threaded main loop**: All event sources (input, ticks, DB polls, agent output) share one mpsc channel. Agent events compete with key events for processing time.
2. **Global agent state**: Only one agent session tracked at a time (`agent_session_id`, `agent_running` are global on `AppState`). Need per-worktree agent state for concurrent agents.
3. **Embedded output**: Streaming agent output through the TUI event loop adds load proportional to agent verbosity.

---

## Architecture Evolution (v1.1)

Two changes to address the scalability concerns above, in priority order.

### Change 1: Priority Input Channel

**Problem**: Key events and background events share one mpsc channel. During heavy background activity, key events queue behind background events, causing input lag.

**Solution**: Separate channels for input vs background. The main loop always drains the input channel first.

```
┌──────────────────────────────────────────────────┐
│  Main Loop                                       │
│                                                  │
│  1. Drain input_rx (all keys)    ◄── HIGH PRIO   │
│  2. Drain bg_rx (backgrounds)    ◄── LOW PRIO    │
│  3. Draw if dirty                                │
│  4. Block on either channel                      │
│                                                  │
│  input_rx ◄── Input thread (10ms poll)           │
│  bg_rx    ◄── DB poller, agent threads, ticks    │
└──────────────────────────────────────────────────┘
```

Key events are always processed before any background events within a single loop iteration. Even with 10 agents flooding `bg_rx`, a keystroke on `input_rx` is handled immediately.

### Change 2: tmux-Based Agent Execution

**Problem**: Streaming agent output through the TUI event loop doesn't scale to N concurrent agents. The TUI becomes a bottleneck for I/O that doesn't need to flow through it.

**Solution**: Launch agents in tmux panes instead of capturing their stdout. The TUI monitors agent status via the DB, not via streaming output.

```
┌───────────────┐     ┌──────────────────────────┐
│  TUI          │     │  tmux session            │
│               │     │                          │
│  Worktree A   │────►│  pane 1: claude agent A  │
│  [running]    │     │  pane 2: claude agent B  │
│               │     │  pane 3: claude agent C  │
│  Worktree B   │     │                          │
│  [done]       │     └──────────────────────────┘
│               │
│  Worktree C   │     Agent status comes from DB,
│  [running]    │     not from streaming output.
└───────────────┘
```

**Agent launch flow**:
1. User presses `r` on a worktree
2. TUI creates an `agent_runs` DB record
3. TUI spawns `tmux new-window -t conductor -n <worktree-slug> -- claude -p <prompt> ...`
4. Claude runs in its own tmux pane with full terminal capabilities
5. A small wrapper script updates the DB when the process exits (status, cost, session_id)
6. TUI polls `agent_runs` table to show status — zero streaming events through the event loop
7. User presses `a` to attach to that agent's tmux pane

**Benefits**:
- Zero TUI event loop load per agent — scales to any number of concurrent agents
- Agents get a real terminal — full interactive capability if needed
- Agent output is preserved in tmux scrollback — no buffer cap needed
- Clean separation: TUI = orchestration, tmux = execution
- Users can detach/reattach to agents independently of the TUI

**DB-driven status tracking**:
The wrapper script (or a post-exit hook) updates the `agent_runs` row:
```bash
#!/bin/bash
claude -p "$PROMPT" --output-format json --verbose --permission-mode acceptEdits \
  ${RESUME:+--resume "$RESUME"} \
  > /tmp/conductor-agent-$RUN_ID.json 2>&1
# Parse result and update DB
conductor agent complete --run-id "$RUN_ID" --result-file /tmp/conductor-agent-$RUN_ID.json
```

Or directly via the Rust CLI:
```bash
conductor agent run --worktree-id <id> --prompt "..." [--resume <session-id>]
```

This `conductor agent run` subcommand would:
1. Create the DB record
2. Run `claude -p` to completion
3. Parse the JSON result
4. Update the DB record with status, session_id, cost, etc.

The TUI's `r` key would then spawn: `tmux new-window -- conductor agent run --worktree-id <id> ...`

---

## Future: v2 Daemon Extraction

See `docs/SPEC.md` section "v2: Daemon Extraction" for the full analysis. The daemon becomes the single owner of agent processes, DB writes, and state. The TUI becomes a thin display client connected via IPC. This is the long-term path for real-time multi-client state sync, but the v1.1 changes above address the immediate scalability needs without the architectural overhead.

---

## Reference: Claude CLI Flags

```bash
# Non-interactive execution
claude -p "Your prompt here"

# Output formats
claude -p "task" --output-format json         # Single JSON result
claude -p "task" --output-format stream-json   # Real-time JSON stream (requires --verbose)

# Session management
claude -p "Continue" --resume <session-id>
claude -p "Fork" --fork-session <session-id>

# Permission modes
claude -p "task" --permission-mode plan           # Read-only analysis
claude -p "task" --permission-mode acceptEdits     # Allow file writes
claude -p "task" --permission-mode bypassPermissions  # Allow everything (sandboxed only)

# Tool control
claude -p "Review" --allowedTools "Read,Glob,Grep"

# Custom system prompts
claude -p "task" --append-system-prompt "Custom instructions"

# Worktree + tmux integration (built-in)
claude --worktree feat-name --tmux
```

## Billing Note

The `claude` CLI uses the existing Claude Code subscription auth. No separate API key needed. The Claude Agent SDK (Python/TypeScript) and direct Anthropic API calls require a separate `ANTHROPIC_API_KEY` from [console.anthropic.com](https://console.anthropic.com) with usage-based billing.
