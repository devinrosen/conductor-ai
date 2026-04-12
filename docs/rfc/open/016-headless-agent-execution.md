# RFC 016: Headless Agent Execution

**Status:** Draft
**Date:** 2026-04-10
**Author:** Devin

---

## Problem

Conductor spawns all agents — from the TUI, web, desktop, and workflow engine — by creating a tmux window and running `conductor agent run` inside it. This approach was the right call early on: it gave users a real interactive terminal to watch and interact with agents as they worked. But as conductor has grown to include a web UI, a native desktop app, and a workflow engine that runs dozens of agents autonomously, the tmux substrate has become a liability.

### Costs we are paying today

**1. The orphan reaper** (#277) — tmux windows can disappear without conductor's knowledge: a user closes a terminal, tmux is killed, the machine sleeps and wakes. When this happens, agent runs get permanently stuck in `running` status. The orphan reaper was built entirely to work around this: it polls tmux window names and marks runs as failed when their window is gone. This complexity exists solely because the process lifecycle is owned by tmux, not conductor.

**2. 5-second polling lag** — because the agent runs inside tmux, conductor has no direct visibility into what it's doing. Status changes are detected by polling the database every 5 seconds. A workflow step that completes in 90 seconds may sit in `running` state for another 4.9 seconds before the TUI or web updates. Live turn counts are estimated by scanning a log file byte-by-byte on each tick.

**3. No streaming in the web or desktop UI** — tmux gives no programmatic output stream. The web and desktop UIs cannot show live agent progress. There is no terminal to attach to from a browser or a native window.

**4. Tmux is a hard dependency** — every environment that runs conductor must have tmux installed. Conductor-desktop (Tauri native macOS) embeds a web server and exposes the same web UI; tmux provides no benefit there but remains a hard dependency.

**5. "Press `a` to attach" only works in the TUI** — the interactive terminal attachment feature is unavailable from the web or desktop. For those surfaces, tmux delivers all the downside with none of the upside.

**6. Prompt file leakage** — when prompts exceed ~512 bytes (always, in practice), conductor writes them to `{worktree_path}/.conductor-prompt-{run_id}.txt` and never cleans them up. This is a workaround for tmux's command-line length limit.

---

## Proposed Design

Replace tmux-based spawning with direct subprocess management: conductor owns the `claude` process, reads its `--output-format stream-json` stdout line-by-line, and propagates structured events to the DB, TUI, and web in real time.

```
Before (tmux):

conductor ──► tmux new-window ──► [tmux session]
                                       │
                                conductor agent run
                                       │
                                     claude
                                       │
                                stdout → log file
                                ← DB poll every 5s ─────────── conductor

After (headless):

conductor ──► spawn subprocess ──► conductor agent run
                  │                        │
              owns stdout pipe           claude
                  │                        │
              stream-json events    stdout → pipe → conductor
                  │
              real-time DB writes + TUI/web callbacks
```

### 1. Subprocess spawn

Replace `spawn_tmux_window()` in `conductor-core/src/agent_runtime.rs` with `spawn_headless()`:

```rust
pub struct HeadlessHandle {
    /// OS process ID — stored in DB for orphan detection and cancellation.
    pub pid: u32,
    pub stdout: std::process::ChildStdout,
    pub stderr: std::process::ChildStderr,
}

pub fn spawn_headless(
    args: &[&str],
    working_dir: &Path,
) -> Result<HeadlessHandle, String> {
    let child = Command::new("conductor")
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)   // own process group — survives terminal close (SIGHUP)
        .spawn()
        .map_err(|e| format!("Failed to spawn conductor: {e}"))?;

    Ok(HeadlessHandle {
        pid: child.id(),
        stdout: child.stdout.unwrap(),
        stderr: child.stderr.unwrap(),
    })
}
```

`process_group(0)` gives the child its own process group, so it will not receive SIGHUP when the user closes the terminal — the same process resilience guarantee as tmux, without the extra indirection.

### 2. PID tracking

Store the subprocess PID in the DB immediately after spawn:

```sql
-- Migration 064
ALTER TABLE agent_runs ADD COLUMN subprocess_pid INTEGER;
```

`tmux_window` is kept (nullable) for backward compatibility. Existing completed runs retain their window names; new headless runs have `tmux_window = NULL` and `subprocess_pid = <pid>`.

The PID enables:
- **Orphan detection** — `kill -0 <pid>` to check liveness
- **Cancellation** — SIGTERM to the process group
- **TUI/web reconnect** — a restart can see that a run is still in progress

### 3. Stdout event loop

The caller reads the subprocess stdout and propagates events in real time:

```rust
pub fn drain_stream_json(
    stdout: ChildStdout,
    run_id: &str,
    log_file: &Path,
    conn: &Connection,
    on_event: impl Fn(&AgentEvent),
) -> Result<AgentRunResult> {
    let reader = BufReader::new(stdout);
    let mut log = OpenOptions::new().append(true).create(true).open(log_file)?;

    for line in reader.lines() {
        let line = line?;
        writeln!(log, "{line}")?;                        // persist to log file (unchanged)

        if let Some(event) = parse_stream_json_line(&line) {
            on_event(&event);                            // live TUI / web callback
            apply_event_to_db(conn, run_id, &event)?;   // eager DB write per event
        }
    }

    // EOF — subprocess has exited
    Ok(collect_final_result(conn, run_id)?)
}
```

`apply_event_to_db` writes incrementally:
- `system/init` event → store `model`, `claude_session_id`
- `assistant` event → update partial token counts
- `result` event → store `result_text`, `cost_usd`, `num_turns`, `duration_ms`, token counts, `status = completed/failed`

This replaces both the 5-second DB poll and the byte-offset log scan as the primary update mechanism.

### 4. Orphan detection

Replace `list_live_tmux_windows()` with a PID liveness check:

```rust
fn pid_is_alive(pid: u32) -> bool {
    // kill(pid, 0) sends no signal; returns Ok if process exists and we own it
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
```

The orphan reaper queries runs with `status = 'running'`:
- If `subprocess_pid IS NOT NULL` → check PID liveness
- If `tmux_window IS NOT NULL` → check tmux window (existing path, handles pre-migration runs)
- If neither → mark failed immediately (should not happen in practice)

Both checks coexist during the transition period so pre-migration runs are still reaped correctly.

### 5. Cancellation

Replace `tmux kill-window` with SIGTERM to the process group:

```rust
pub fn cancel_subprocess(pid: u32) {
    // Negative PID targets the entire process group (claude + any children it spawned)
    unsafe { libc::kill(-(pid as i32), libc::SIGTERM); }
}
```

The TUI `handle_stop_agent()` sends SIGTERM, waits briefly for graceful shutdown, then SIGKILL if the process is still alive. The DB update (`status = cancelled`) happens in the same background thread as today.

### 6. Feedback (no changes required)

The feedback mechanism is already subprocess-agnostic and requires no changes:

1. `conductor agent run` detects `@@feedback:` markers in the Claude stream-json output
2. Creates a `feedback_requests` row; sets run status to `waiting_for_feedback`
3. Polls the DB every 2 seconds for a response from the TUI or web
4. On response: spawns `claude --resume {session_id} -p {response}` for the next turn

This loop runs entirely inside `conductor agent run`. Conductor's ownership of the subprocess stdout pipe does not interfere — the pipe simply carries no new lines while the CLI waits for feedback. TUI and web present the feedback modal and write to the DB exactly as they do today.

### 7. Live streaming to TUI and web

The `on_event` callback in `drain_stream_json` provides a real-time hook for each surface:

**TUI:** The thread draining the subprocess stdout fires `Action::AgentEvent { run_id, event }` into the existing `bg_tx` channel. The TUI main thread renders on each assistant message, tool use, and result event. The 5-second DB poll continues as a consistency backstop but is no longer the primary update path.

**Web:** The web server drains the subprocess stdout in a `tokio::task::spawn_blocking` task. Events are broadcast to SSE subscribers via a per-run `tokio::sync::broadcast` channel. The browser receives structured events and renders a live activity feed without polling.

**Desktop:** Inherits the web streaming path transparently — the Tauri wrapper embeds the web server unchanged.

### 8. Prompt handling

Prompts are always written to a temp file and cleaned up on completion — no more 512-byte threshold, no more leaked files in worktree directories:

```rust
let prompt_file = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
std::fs::write(&prompt_file, &prompt)?;
// ... spawn, drain to completion ...
let _ = std::fs::remove_file(&prompt_file);   // always cleaned up
```

### 9. "Attach to agent" replacement

The TUI's `press a to attach` opens the agent's tmux window so the user can watch in real time. In the headless model, conductor owns the output stream and renders it directly.

**Phase 1 (this RFC):** Remove the attach keybinding. The TUI's worktree detail panel shows live event updates as they arrive via the `on_event` callback — assistant messages, tool uses, and intermediate results appear in-product. The user watches agent progress without leaving conductor.

**Phase 2 (future, out of scope):** A scrollable "agent log" panel renders the full event history for a run with syntax highlighting and tool-call formatting. This is a pure UI addition requiring no backend changes.

### 10. Script role steps

Workflow steps with `role = "script"` run arbitrary shell commands, not the Claude CLI. These also currently use `spawn_tmux_window`. They should be migrated to direct subprocess spawn with the same PID-tracking and cancellation approach, keeping existing stdout/stderr capture into the log file.

---

## DB Schema Changes

```sql
-- Migration 064: subprocess_pid for headless agent tracking
ALTER TABLE agent_runs ADD COLUMN subprocess_pid INTEGER;

-- tmux_window stays nullable — kept for pre-migration runs.
-- New headless runs: tmux_window = NULL, subprocess_pid = <pid>.
```

No other schema changes. Log file path, result columns, and token columns are unchanged.

---

## Decisions Made

1. **`process_group(0)` for resilience.** Child runs in its own process group and does not receive SIGHUP on terminal close. Equivalent guarantee to tmux without the indirection.

2. **PID in DB, not a pidfile.** Consistent with how `tmux_window` is stored. Accessible to any process (TUI, web, CLI, MCP) without filesystem coordination.

3. **`tmux_window` column kept, not dropped.** Backward compatible. Old runs keep their window names. The orphan reaper handles both column patterns during the transition.

4. **Feedback mechanism unchanged.** Already subprocess-agnostic. Changing it would be scope creep with no benefit.

5. **"Attach" removed in Phase 1, replaced with live in-product rendering.** The web and desktop never had attach. Replacing it with live event streaming is a better experience across all surfaces. A dedicated log panel is Phase 2.

6. **Prompt files always in temp dir, always cleaned up.** Removes the 512-byte threshold and the prompt file leakage problem.

7. **Script role steps also migrated.** Keeping a tmux path for scripts while removing it for agents would leave a split model with no benefit.

8. **5-second DB poll kept as backstop.** The real-time event path is primary; the poll remains for consistency checks and handles edge cases (crashed conductor process, SSE client reconnect).

---

## Open Questions

1. **PID reuse.** OS PIDs are recycled. A PID stored in the DB could, after the agent exits, belong to an unrelated process. The orphan reaper should cross-check `started_at` against the process creation time from `/proc/{pid}/stat` (Linux) or `sysctl KERN_PROC` (macOS) to avoid false positives.

2. **SIGTERM handling in Claude CLI.** ~~Does `claude` flush its final `result` event to stdout before exiting on SIGTERM?~~ **Resolved (empirically tested 2026-04-10):** No. When SIGTERM is sent while Claude is executing a tool, the stream ends at EOF with no `result` event — the pipe simply closes. The cancellation path must therefore mark the run as `cancelled` directly in the DB after sending SIGTERM, rather than waiting for a `result` event that will never arrive. Log-based recovery is not appropriate for intentional cancellation. `drain_stream_json` must handle the no-result EOF case explicitly and return a synthetic `AgentRunResult { status: Cancelled, ... }`.

3. **Parallel stdout multiplexing.** Parallel workflow blocks spawn multiple agents concurrently. Each agent's stdout must be drained on its own thread. The current parallel executor already uses threads; each thread should own one pipe. Worth explicit testing under load.

4. **Windows support.** `process_group(0)` and `libc::kill` are Unix-only. Conductor currently assumes Unix (tmux is also Unix-only), so this is not a blocker. If Windows becomes a target, process management needs a platform abstraction layer.

5. **Incremental token streaming during long runs.** Currently the TUI scans log bytes to estimate partial token usage mid-run. With headless, each `assistant` event carries token counts directly. The `on_event` callback can update the DB incrementally, making mid-run token display exact rather than estimated. Should be implemented in the same PR.

---

## Implementation Order

Steps 1–6 land as a single infrastructure PR with no behavioral change (tmux path still used by callers). Steps 7–11 are the switching PR. Steps 12–14 are cleanup.

1. DB migration — add `subprocess_pid` column (nullable, backward compatible)
2. `spawn_headless()` — implement in `agent_runtime.rs`; keep `spawn_tmux_window()` in place
3. `drain_stream_json()` — event loop with log writing, eager DB writes, `on_event` callback
4. `cancel_subprocess()` — SIGTERM to process group
5. Orphan reaper — add PID liveness check alongside existing tmux window check
6. Prompt file cleanup — temp dir, delete on completion
7. Workflow executors (`call.rs`, `parallel.rs`) — switch to `spawn_headless`
8. Orchestrator — switch to `spawn_headless`
9. TUI agent launch — remove tmux spawn, wire live event callback to `bg_tx`
10. Web agent launch — remove `spawn_tmux_blocking`, wire live events to SSE broadcast
11. Remove "attach" keybinding — replace with live event display in worktree detail panel
12. Script role steps — migrate to direct subprocess
13. Remove `spawn_tmux_window()` — once all callers migrated
14. Remove tmux window check from orphan reaper — after a defined grace period for pre-migration runs

---

## Out of Scope

- **Agent log viewer panel in TUI** (Phase 2 of "attach" replacement) — pure UI work, no backend dependency
- **RFC 007 multi-runtime agents** — the `AgentRuntime` trait proposed there maps naturally onto the headless model, but RFC 007 is a separate design
- **Streaming tool-use events to TUI panels** — the `on_event` callback enables this; rendering is a separate feature
- **Removing tmux from the prerequisites list entirely** — users may still want tmux for their own workflow; conductor just won't depend on it
- **Windows support**
