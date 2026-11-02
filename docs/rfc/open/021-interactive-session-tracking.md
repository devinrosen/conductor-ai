# RFC 021: Interactive Session Tracking

**Status:** Draft
**Date:** 2026-11-02
**Author:** Devin

---

## Problem

Conductor tracks the headless agents it spawns. It knows nothing about the Claude Code sessions a developer runs by hand in a conductor worktree, which is how judgement-heavy tickets get worked. With three or four of those open at once, the developer is back to the overhead conductor exists to remove: remembering which terminal holds which ticket, which session is blocked on a permission prompt, and which finished its turn and is waiting for them.

A stopgap exists outside conductor: a personal Claude Code plugin (`devinrosen/claude-plugin-work`) that launches each ticket's session in a named tmux window, colours the window from Claude Code hooks (yellow on a permission prompt, green when a turn ends) and reports status by reading the panes. It duplicates conductor's worktree and ticket tracking, and none of it reaches the TUI, the web app or a phone.

## Goals

- Every interactive Claude Code session in a conductor worktree shows up in the TUI and web with its current state: working, waiting on a permission prompt, idle (the developer's turn), or ended.
- Conductor notifies when a session is blocked or has finished its turn, and says which worktree it is.
- Conductor launches, resumes and focuses a worktree's session from the TUI, the CLI and MCP.
- Each session has cost and token totals and a per-turn activity log.
- None of this slows down or breaks the developer's session.

## Non-Goals

- Answering permission prompts from conductor, the web or a phone. The first version only notifies: the developer answers in the terminal. See [Future](#future).
- Typing into a session from conductor, beyond the start prompt at launch.
- Tracking sessions outside conductor worktrees, including a repo's main checkout.
- Interactive tools other than Claude Code.
- A terminal in the web UI.

## Proposed Design

### Overview

```
claude (interactive, cwd in a conductor worktree)
  └─ Claude Code hook (conductor plugin)
       └─ hooks/conductor-session-hook          exits silently if it doesn't apply
            └─ conductor session hook           one small write per event
                 ├─ SQLite: agent_runs, feedback_requests, agent_run_events
                 └─ session-spool/<session_id>.jsonl   when SQLite is busy

TUI and web pollers (existing)
  ├─ show state            worktree symbols, detail view, Attention Required
  ├─ reap                  dead claude PID → failed
  ├─ notify                feedback.requested, session.idle
  ├─ drain spool files
  └─ ingest metrics        from the session transcript
```

An interactive session is an `agent_runs` row with `runtime = 'interactive'`. Claude Code hooks report its state through a CLI command that writes the database. Everything downstream reuses what conductor already has for headless runs: the run table, feedback requests, the run event table, the pollers, the orphan reaper, the notification hooks, the TUI symbols and the web's Attention Required. This follows TASTE.md's "prefer composition over parallel systems" and DIRECTION.md's library-first, no-daemon model: the hook is a short-lived CLI process and the pollers are the ones that already run.

### 1. Data model

There is **one `agent_runs` row per Claude Code session**, keyed by `session_id`. `claude --resume <id>` keeps the same `session_id` (Spike C), so resuming reopens the same row, and the worktree shows one current state.

| Column | Interactive meaning |
|---|---|
| `runtime` | `'interactive'` |
| `session_id` | Claude Code's session id, stable across `--resume` |
| `worktree_id`, `repo_id` | the active worktree containing `cwd` |
| `status` | `running`, `waiting_for_feedback`, **`idle`** (new), `completed` (the session ended), `failed` (reaped after a crash). Never `cancelled`: conductor doesn't stop interactive sessions. |
| `prompt` | the latest user prompt, truncated |
| `result_text` | the latest `last_assistant_message` from `Stop` |
| `subprocess_pid` | the claude process (`CLAUDE_PID`) |
| `started_at` | when the session first started; never reset on resume |
| `ended_at` | `SessionEnd`, or when it was reaped |
| `model` | from `SessionStart` |
| `cost_usd`, token columns, `duration_ms`, `num_turns` | session totals from the transcript (see [section 9](#9-metrics)) |
| **`idle_since`** (new) | set by `Stop`, cleared by `UserPromptSubmit`; drives the idle display and `session.idle` |
| **`subprocess_started_at`** (new) | start time of the current claude process, for the reaper's PID-reuse guard |
| **`tmux_pane`** (new) | `TMUX_PANE` from the hook environment, so the launcher can focus the session |
| **`transcript_path`** (new) | from every hook payload; read for metrics and denial detection. `log_file` keeps its headless meaning, because the reaper's `try_recover_from_log` parses it as stream-json. |

`num_turns` counts user turns (prompts) for interactive runs. Headless runs report Claude's agentic turn count there, so the two aren't comparable.

**Per-turn activity** goes into the existing `agent_run_events` span table (`kind`, `summary`, `started_at`, `ended_at`, `metadata`), using kinds the TUI's Agent Activity panel already renders. The turn's `prompt_id` goes in `metadata`. Spike C's Agent Activity read "No agent activity" because the spike wrote no events.

### 2. State machine

| Hook | Status | Other writes | Event (`kind`: summary) |
|---|---|---|---|
| `SessionStart` (`startup`, `resume`) | `idle`, with `idle_since` left NULL | Upsert the row on `session_id`. Set `subprocess_pid`, `subprocess_started_at`, `tmux_pane`, `transcript_path` and `model`. A resume reopens a `completed` or `failed` row. | `system`: "Session started" or "Session resumed" |
| `UserPromptSubmit` | `running` | Dismiss pending requests; clear `idle_since`; set `prompt` | `prompt`: the prompt (span starts; `prompt_id` in metadata) |
| `PermissionRequest` | `waiting_for_feedback` | Insert a `feedback_requests` row "Permission: `<tool>` `<command or file_path>`" with a natural key, so a spool replay can't duplicate it | `system`: "Waiting: Permission …" |
| `PostToolUse` | `waiting_for_feedback` → `running` | Mark pending requests `responded` ("answered in terminal") | `tool`: `<tool>` `<summary>` (with `duration_ms`) |
| `PostToolUseFailure` | as `PostToolUse` | as `PostToolUse` | `tool_error`: `<tool>` |
| `Stop` | `idle` | Set `idle_since`; `result_text` = `last_assistant_message`; dismiss pending requests | `result`: the last message (the prompt span ends; the `background_tasks` count goes in metadata) |
| `SessionEnd` | `completed`, with `ended_at` | Dismiss pending requests; clear `idle_since` | `system`: "Session ended (`<reason>`)" |

`PreToolUse`, `Notification` and `SubagentStop` aren't registered. `PermissionRequest` is the waiting signal: it fires the moment the prompt appears (Spike B). `Notification` is late, and `SubagentStop` fires for Claude Code's internal agents.

**Not yet observed:** `SessionStart` with `source` `clear`, `compact` or `fork`, and whether `/clear` keeps the `session_id`. See open questions.

### 3. The hook command

`conductor session hook` is a hidden CLI subcommand. It reads one hook payload on stdin and applies one row of the state machine.

- **Fast start.** It skips `check_prerequisites()`, which spawns `gh`, `tmux` and `claude --version`, and skips config loading. It opens the database with the compat migration check (`open_database_compat`), so a database migrated by a newer binary still works. Spike C measured **12.7 ms p50 wall time**, about 6 ms of it opening and writing, against about 40 ms for a normal CLI command.
- **Only where it applies.** It does nothing unless `cwd` is inside an **active** conductor worktree (the longest matching worktree path). That excludes main checkouts, unregistered projects, and merged or abandoned worktrees. A `SessionEnd` for an unknown session is ignored.
- **The claude PID** comes from `CLAUDE_PID`, which Claude Code exports to hooks; `getppid()` is the fallback. Behind the plugin's wrapper script, `getppid()` would be the shell.
- **Never blocks, never loses events** (#3160):
  - Write with a short busy timeout, about 150 ms instead of the usual 5 s.
  - On `SQLITE_BUSY` or any database error, append a trimmed event to `<CONDUCTOR_HOME>/session-spool/<session_id>.jsonl` and exit.
  - Once a session has a spool file, its events keep going there, even when the database is free, so they stay in order.
  - The next hook for that session and the TUI and web pollers drain the file by renaming it first. They apply its events in order, with each event's own timestamp, in one transaction.
  - Replays are idempotent: the feedback request natural key above is what prevents a duplicate request.
- **Always exits 0** with no stdout. Claude Code surfaces a failing hook in the developer's session on every tool call.

### 4. Plugin and installation

The hooks ship in a **new `conductor` Claude Code plugin in this repo** (#3161). It is not the `conductor` plugin in `LivelyVideo/claude-plugin-marketplace`.

```
.claude-plugin/marketplace.json      # this repo is a marketplace: "conductor-ai"
plugin/
  .claude-plugin/plugin.json         # name "conductor"; version = the conductor release
  hooks/hooks.json                   # SessionStart, UserPromptSubmit, PermissionRequest,
                                     # PostToolUse, PostToolUseFailure, Stop, SessionEnd
  hooks/conductor-session-hook       # wrapper
  README.md
```

**Install:**
1. `claude plugin marketplace add devinrosen/conductor-ai` (the repo is public).
2. `claude plugin install conductor@conductor-ai`.

`conductor setup install` prints these commands, or runs them on request.

**The first version ships hooks only.** The MCP server stays registered by `conductor setup install`, and the workflow skills stay as project skills in `.claude/skills`. The plugin's version is bumped in the release commit, so the plugin and the binary it calls come from the same release.

**The wrapper** runs in every Claude Code session on the machine, on every tool call, so it must be a silent no-op whenever it doesn't apply:

- **Headless sessions.** Skip unless `CLAUDE_CODE_SESSION_ATTENDED=1`. Under `claude -p` it is `0` and `CLAUDE_CODE_ENTRYPOINT` is `sdk-cli` (verified). This check is what keeps conductor's own workflow agents out: runkon-runtimes doesn't set `CONDUCTOR_RUN_ID` on them.
- **`conductor` not on `PATH`:** exit 0.
- **A binary too old to have the subcommand:** `conductor session hook 2>/dev/null || true`.
- **Optionally,** a cheap pre-filter on `$PWD` under the workspace root, before starting the binary at all.

### 5. Launching and resuming sessions

Conductor launches sessions as well as observing them (#3158). One core function backs three surfaces:
- a TUI key on a worktree ("open session");
- `conductor session open <repo> <worktree> [--new] [--prompt <text> | --prompt-file -]`;
- an MCP tool, `conductor_open_session`.

**Behaviour:**
1. **The session is live:** focus its pane, using `tmux_pane`.
2. **The worktree has a past session:** open a window with `claude --resume <newest session_id>`.
3. **Otherwise:** open a fresh `claude`.

`--new` skips step 2.

**Details:**
- **tmux session:** configurable, default `work`. If it doesn't exist, conductor creates it detached and shows `tmux attach -t work`.
- **Window name:** the linked ticket's key, lowercased, or the worktree slug when no ticket is linked.
- **Start prompt:**
  - **Fresh session with a linked ticket:** an opener generated from a template that can be overridden per repo.
  - **No ticket:** a blank session.
  - **Resume:** no prompt by default.
  - `--prompt` always overrides.
- **Focus outside tmux:** if the TUI runs inside the target tmux session, switch to the window. Otherwise show and copy `tmux attach -t <session> \; select-window -t <window>`.
- **Hooks:** the launcher relies on the plugin. It passes the hooks with `--settings` only when the plugin isn't installed, so events never fire twice.
- **TUI threading:** tmux calls run off the TUI main thread, per the threading rule.

Spike A measured launch at **0.6 s to the prompt** for a fresh session and **1.7 s** for `--resume` of a 10 MB transcript.

**The `work` plugin** shrinks to its manager skill (triage, overlap check, naming) and calls `conductor session open` or `conductor_open_session`. Its tmux-colouring hooks and pane-scraping status go away. **Taking over a headless run's session** by hand is a separate, explicit action (#3163).

### 6. Orphan reaper

`reap_orphaned_runs` already checks every active run's PID. For interactive runs:

- **`idle` counts as active,** so a session killed while idle is reaped. Today a crash while idle would go unnoticed.
- **The PID-reuse guard** compares the process start time against `subprocess_started_at` rather than `started_at` (#3155). A resumed session is a new process on an old row, so comparing against `started_at` reaps live resumed sessions; Spike C reproduced it.
- **A dead PID:** the run becomes `failed` with `ended_at` set. The run also needs two fixes Spike C found:
  - Keep `result_text`, the last assistant message, and record the failure reason in a `system` event instead of overwriting it.
  - Dismiss any pending feedback requests. Today they stay pending forever.
- **No `try_recover_from_log`** for interactive runs: there is no stream-json log.

### 7. Stop, respond and dismiss

- **Stop:** `AgentManager::cancel_run` refuses `runtime = 'interactive'`, because it signals `subprocess_pid`, which is the developer's claude (#3154). Spike C killed a session with the TUI's `x`. The TUI offers "focus session" instead of stop, and the web hides Stop for interactive runs.
- **Respond:** hidden for interactive permission requests in the TUI, the web and `conductor_submit_agent_feedback`. Nothing would read the response until the phone-answer phase.
- **Dismiss:** kept. It's the manual way to clear a request left stale by a denied permission (see [section 11](#11-denied-permissions)).

### 8. Notifications

**Prerequisites on `main`** (bugs today, independent of this RFC):
- **#3152:** `feedback.requested` and gate hooks never fire unless `[notifications] enabled = true`.
- **#3153:** the `feedback.requested` payload doesn't name the worktree, and its `run_id` field holds the request id.
- **#3162:** Web Push never fires for gates, and fires for feedback only through the web's own route. Database-written requests never reach the event bus.

**Permission prompts** use the existing `feedback.requested` kind, so they reach notification hooks and Web Push as headless feedback requests do, once those fixes land.

**`session.idle`** is a new event kind for "the turn ended, it's your move" (#3159):
- **When it fires:** after `delay_secs` of continuous idle. That's `[notifications.session_idle] delay_secs`, default 30 s; `0` fires immediately.
- **How often:** once per idle period, deduplicated on `(run_id, idle_since)`.
- **Not after `SessionStart`:** a session that hasn't had a turn doesn't ping.
- **Background tasks:** it still fires when they're running, and carries their count.
- **Who fires it:** the pollers, not the hook, with the same gate fix as #3152.
- **Payload:** the worktree, branch, `session_id`, a prompt preview and a last-message preview.
- **Web Push:** opt-in with `web_push = true`. By default Web Push sends only `feedback.requested` and gate-waiting. Hooks choose events with their own `on` filter, and there is no other toggle.

**`agent_run.completed` is suppressed for interactive runs.** Every `SessionEnd` would otherwise fire "Agent completed" for a session the developer just closed. **`agent_run.failed` still fires**, which for interactive runs means the session crashed and was reaped. The pollers' `detect_agent_terminal_transitions` callers skip `completed` transitions when `runtime = 'interactive'`.

### 9. Metrics

No hook reports tokens or cost; the session transcript (`transcript_path`) does. A poller reads it when a run goes idle or ends, never the hook itself.

- **Session totals** come from the latest `cost-state` entry: `totalCostUSD`, API and tool durations, lines changed, and per-model token counts. They're cumulative across resumes and written to `cost_usd`, the token columns and `duration_ms`.
- **Per-turn tokens** come from assistant messages' `usage`, deduplicated on `message.id`. They're grouped into turns through the `parentUuid` chain to the user entry that carries `promptId`, and written into the turn's `result` event metadata.
- **Fallback:** sum the per-message usage when there's no final `cost-state` entry (after `kill -9`).
- **Reconciliation:** per-message sums don't match `cost-state` (input 16 vs 2,252 tokens in Spike C), probably because `cost-state` also counts Claude Code's side calls. Session totals use `cost-state`. Treat per-turn figures as approximate until reconciled.
- **On resume,** the `SessionStart` payload's `context_tokens` and `estimated_cache_write_usd` can be shown as "resuming will cost about $X".
- **Read incrementally,** from `transcript_offset`, because transcripts reach 10 MB and more (#3165).

Both transcript formats are internal to Claude Code and undocumented. The parser must tolerate missing fields and degrade to "no metrics" rather than fail. If the metric columns move off `agent_runs` (runkon/runkon#6), interactive metrics move with them.

### 10. TUI and web changes

**TUI:**
- **Idle symbol:** a new symbol, distinct from `⚙`, `⏸` and `✓`.
- **Worktree detail:**
  - While waiting: "waiting: Permission: Bash … since HH:MM".
  - While idle: "idle since HH:MM" and the last message.
  - The session cost.
- **Agent Activity:** shows the per-turn events with no change.
- **Keys:** "open session" also focuses a live session, and replaces stop for interactive runs.

**Web:**
- **Idle badge:** in `StatusBadge`, `StatusPulseBadge` and `SignalLight`, and counted in `agentStats`.
- **Attention Required:** lists `waiting_for_feedback` only, never idle. Respond is hidden for interactive runs and Dismiss is kept.
- **Stop:** hidden for interactive runs.
- **Launching:** not available from the web, which has no terminal. It can show the attach command.

### 11. Denied permissions

Choosing "No" at a permission prompt interrupts the turn, and **no hook fires**: no `PostToolUse`, `PostToolUseFailure`, `Stop`, `PermissionDenied` or `StopFailure` (Spike C). The run stays `waiting_for_feedback` until the next `UserPromptSubmit` or `SessionEnd`. The transcript does record the denial, as a `tool_result` with `toolUseResult: "User rejected tool use"` followed by `[Request interrupted by user for tool use]`.

The handling is staged (#3156):

1. **First version: accept it and make it visible.** The TUI and web show "waiting since HH:MM", so a long wait reads as suspicious, and Dismiss clears the request by hand. The next prompt clears it automatically.
2. **With metrics ingestion (Phase 5): detect it from the transcript.** The same incremental reader that ingests metrics also looks, for runs with a pending permission request, for a rejection or interrupt entry after the request's `created_at`. When it finds one, it dismisses the request and moves the run to `idle`. That costs nothing extra, because the reader and its offset already exist. It shares the transcript-format risk noted in section 9.
3. **In parallel: ask upstream** for a hook on a user denial or interrupt, for example `PermissionDenied` firing for interactive denials, or `Stop` with an `interrupted` flag. If one ships, it replaces step 2.

## DB Schema Changes

One migration, **087** (#3157):

- **`agent_runs` rebuilt** with `'idle'` added to the `status` CHECK constraint. SQLite can't alter a CHECK, so the table is rebuilt as in migration 027, with its indexes and the `trg_agent_run_delete_cascade_workflow_runs` trigger recreated.
- **New nullable columns on `agent_runs`:** `idle_since`, `subprocess_started_at`, `tmux_pane`, `transcript_path` and `transcript_offset` (the byte offset for incremental reads).
- **New on `feedback_requests`:** a nullable `dedup_key` with a unique index, so a spool replay can't insert a second request.

As with any migration, installed binaries older than the release that adds 087 can't open the database afterwards.

## Decisions Made

1. **Composition, not a parallel system.** An interactive session is an `agent_runs` row with `runtime = 'interactive'`, written by a CLI command that Claude Code hooks call.
2. **Notify-only first.** The developer answers prompts in the terminal; answering from a phone is a later phase.
3. **One run per session** (2026-11-02). Totals live on the row; per-turn detail lives in `agent_run_events`.
4. **A new `idle` status** (2026-11-02, #3157). It's active for the reaper, not stoppable, and kept out of Attention Required.
5. **`session.idle` notifications** (2026-11-02, #3159):
   - a separate event kind from `feedback.requested`;
   - a 30 s default delay, configurable;
   - fires even with background tasks running;
   - Web Push opt-in, and no other toggle.
6. **The hook never blocks on SQLite** (2026-11-02, #3160): a short busy timeout, then a per-session spool.
7. **Hooks ship in a new plugin in this repo** (2026-11-02, #3161). The plugin is versioned with releases and ships hooks only at first. A wrapper skips headless sessions.
8. **Sessions outside active worktrees are ignored** (2026-11-02), including main checkouts.
9. **Conductor launches and resumes sessions** (2026-11-02, #3158). tmux session `work` by default, windows named by ticket key, and generated openers for fresh ticket sessions only. The `work` plugin shrinks to its manager skill.
10. **Taking over a headless run is a separate action** (2026-11-02, #3163).
11. **Denied permissions are handled in stages** (2026-11-02, #3156): visible and dismissible first, then detected from the transcript alongside metrics ingestion, with an upstream hook requested in parallel.
12. **`agent_run.completed` is suppressed for interactive runs; `agent_run.failed` still fires** (2026-11-02).

## Open Questions

1. **`/clear`, compaction and fork.** Does `/clear` start a new `session_id`? Where do `SessionStart` `source = clear | compact | fork` leave the row? Untested. *Expected:* a new session id gets a new row, and compaction keeps the row.
2. **Esc during a turn** is untested. It probably behaves like a denial: an interrupt with no `Stop`.
3. **Several sessions in one worktree.** Two claude windows in the same worktree create two live rows, but the worktree list shows only the latest run, so the other is hidden. The same happens with a workflow agent running where an interactive session is open. Is "newest wins" acceptable, or should the worktree row show a count?
4. **Transcript format stability.** Metrics and denial detection depend on an undocumented format that has changed before. Pin a tested Claude Code version range in the parser and degrade gracefully outside it?
5. **#3163's questions:** which row owns the `session_id` after a takeover; starting in the developer's own permission mode; how the TUI shows the link between the two runs.

## Implementation Order

The phases are ordered so that each one ships something usable. Each phase has a GitHub milestone, "RFC 021 Phase N: …" (milestones 45–51). The spike code on the `feat/spike-interactive-sessions` branch (`conductor-cli/src/handlers/session.rs`, `spike/rfc021/`) is throwaway and not for merge.

**Phase 0: bugs on `main`.** These are independent and can land now: #3152, #3153, #3162.

**Phase 1: schema and core.**
- **#3157:** migration 087, `AgentRunStatus::Idle`, the reaper's status filter, and the TUI and web idle display.
- **#3155:** `subprocess_started_at` in the PID-reuse guard, plus the reaper fixes for `result_text` and pending requests.
- **#3154:** `cancel_run` refuses interactive runs; hide stop and respond.

**Phase 2: the hook command.**
- **#3164:** `conductor session hook` for production: the state machine in section 2, worktree resolution, `CLAUDE_PID`, run events and the feedback natural key.
- **#3160:** the short busy timeout and the spool.

**Phase 3: the plugin.** #3161: the marketplace layout, the hooks and the wrapper, and `setup install` guidance. After this phase, hand-run sessions show up in the TUI and web.

**Phase 4: notifications.** #3159 (`session.idle`) and the `agent_run.completed` suppression for interactive runs, both of which need Phase 0.

**Phase 5: metrics and denial detection.** #3165: incremental transcript ingestion into the run's totals and the turn events, plus the denial check from section 11.

**Phase 6: launching.**
- **#3158:** the core launcher, the CLI, MCP and the TUI key.
- **Shrinking the `work` plugin,** in its own repo.
- **#3163** as a follow-up.

**Denied permissions** (section 11): "waiting since" and Dismiss land with Phase 1's TUI and web changes; transcript detection lands with Phase 5.

## Future

- **Answering from the phone.** A `PermissionRequest` hook can return an allow or deny decision, and conductor's feedback mechanism could carry it: the web or a phone answers, and the hook returns the answer. But a hook that waits for an answer blocks the session by design, which conflicts with section 3. That needs its own design: a bounded wait that falls back to the terminal prompt, and a way to cancel the remote request when the developer answers locally first. TASTE.md requires interactive features to work from a phone browser, so this is the next RFC after this one ships.
- **Other interactive tools,** for any that expose comparable hooks.

## Related

- [RFC 016: Headless Agent Execution](../closed/016-headless-agent-execution.md): the PID-based run tracking and orphan reaper this reuses.
- [RFC 020: Agent Event Indexing](020-agent-event-indexing.md): per-turn events. This RFC uses the existing `agent_run_events` span table.
- [RFC 013: Mobile Push Notifications](013-mobile-push-notifications.md): the relay that would carry `session.idle` and permission prompts off the local network.
- [TASTE.md](../../TASTE.md) and [DIRECTION.md](../../DIRECTION.md).
- **Issues:**
  - #3152, #3153, #3162: notification bugs on `main`;
  - #3154: stop;
  - #3155: reaper on resume;
  - #3156: denials;
  - #3157: idle status;
  - #3158: launcher;
  - #3159: `session.idle`;
  - #3160: spool;
  - #3161: plugin;
  - #3163: takeover;
  - #3164: the hook command;
  - #3165: transcript ingestion.

---

## Appendix: spike results

The design above rests on three spikes run on 2026-11-02, using Claude Code 2.1.316 and Haiku test sessions, against a copy of the conductor database.

### Spike A: launch latency (2026-11-02)

Conductor's old tmux mode was dropped partly because launching felt slow. Measured on Claude Code 2.1.316, from `tmux new-window ... claude` to the input prompt being drawn, with the user's full plugin and MCP server set:

| Launch | Runs | First paint | Prompt ready |
|---|---|---|---|
| Fresh `claude` | 4 | 0.36–0.37 s | 0.60–0.61 s |
| `claude --resume` of a 10 MB transcript | 3 | 0.60–0.79 s | 1.72–1.75 s |

`SessionStart` hooks fire about 0.64 s after the window opens. Launch latency is no longer a reason to keep conductor out of terminals. Not measured: MCP servers may still be connecting after the prompt appears.

### Spike B: hook payloads (2026-11-02)

A Haiku session with logging hooks on every event, driven through a permission prompt, a finished turn, the idle reminder and `/exit`; a second session was killed by closing its tmux window.

```
SessionStart        +0.00s  source=startup
UserPromptSubmit    +0.03s  prompt, prompt_id
PreToolUse          +1.15s  Write
PermissionRequest   +1.18s  tool_name, tool_input, permission_suggestions
Notification        +7.19s  notification_type=permission_prompt, message "Claude needs your permission"
PostToolUse         +12.2s  (after approval) tool_response, duration_ms
Stop                +13.2s  last_assistant_message, background_tasks
SubagentStop        +14.1s  agent_type "" (internal to Claude Code)
Notification        +73.2s  notification_type=idle_prompt (60 s after Stop)
SessionEnd          +73.6s  reason=prompt_input_exit
```

Findings:

1. **Every payload carries `session_id`, `cwd` and `transcript_path`.** `cwd` (and `CLAUDE_PROJECT_DIR` in the environment) maps the session to a worktree.
2. **`PermissionRequest` is the waiting signal, not `Notification`.** It fires the moment the prompt appears and names the tool and its input. `Notification(permission_prompt)` arrives about 6 s later with a generic message, and can land after the developer has already answered.
3. **`Stop` carries `last_assistant_message`**, so conductor can show what the session last said without reading the transcript. `background_tasks` distinguishes idle from idle-with-subagents-running.
4. **Every event within a turn shares a `prompt_id`** (absent on `SessionStart`). Per-turn grouping is free.
5. **No hook reports cost or tokens.** Per-turn or per-session cost means reading `transcript_path`. (Spike C: the transcript carries them; see [section 9](#9-metrics).)
6. **The hook process's parent is the `claude` process**, so a hook can record its PID for the orphan reaper (same PID plus start-time guard as headless runs).
7. **Closing the tmux window still fires `SessionEnd`** with `reason=other`. A crash or SIGKILL presumably doesn't (untested), which the reaper covers.
8. **`TMUX_PANE` is in the hook's environment**, so conductor can record which tmux pane to focus.
9. Ignore `SubagentStop` for session state: one fired after `Stop` for an internal agent.

Not yet observed: a denied permission (probably no `PostToolUse`, so `Stop` and `UserPromptSubmit` must also clear waiting), `SessionStart.source` on `--resume` (expected `resume`), compaction.

### Spike C: hook → database → TUI (2026-11-02)

A throwaway `conductor session hook` subcommand (`conductor-cli/src/handlers/session.rs`) and a `--settings` file (`spike/rfc021/session-hooks.json`) wired to the release binary. It reads the hook payload on stdin, resolves the worktree by the longest active worktree path that contains `cwd`, and upserts an `agent_runs` row with `runtime = 'interactive'` keyed by `session_id`, with the hook's parent PID (claude, confirmed with `ps`) in `subprocess_pid`. No migration. Haiku sessions ran in a separate tmux session against a copy of the real database (schema 86). The TUI and `conductor-web` ran against the same copy, unmodified.

Mapping as built:

| Hook | Write |
|---|---|
| `SessionStart` | insert the run, or reuse the row with this `session_id`; set `subprocess_pid`; then idle |
| `UserPromptSubmit` | dismiss any pending request; `running`; `prompt` = the prompt |
| `PermissionRequest` | `request_feedback("Permission: <tool> <command or file_path>")` → `waiting_for_feedback` |
| `PostToolUse`, `PostToolUseFailure` | mark pending requests `responded`; `waiting_for_feedback` → `running` |
| `Stop` | dismiss pending; idle; `result_text` = `last_assistant_message` |
| `SessionEnd` | dismiss pending; `completed`, `ended_at` |

Idle had three representations, chosen per session by an env var: `completed`, a pending `Idle: <last message>` feedback request, or `running`.

#### Hook cost

Release build, Apple Silicon, 434 MB database. The hook path skips `check_prerequisites()` (three subprocess spawns) and config loading. It opens the database with the compat migration check and runs 1–3 statements.

| Measurement | p50 | p90 | max |
|---|---|---|---|
| Wall time per invocation, 100 × `PostToolUse` from a script | 12.7 ms | 15.9 ms | 26 ms |
| Wall time, 30 × `PermissionRequest` (insert) | 12.6 ms | 14.3 ms | 15 ms |
| In-process time (open + write), 37 hooks from live sessions | 6.0 ms | 153 ms | 684 ms |
| Spawning `/usr/bin/true`, for scale | 2.2 ms | | |
| `conductor repo list`, the normal CLI path with prerequisite checks | ~40 ms | | |

The hot path costs a few milliseconds per tool call, invisible against a model turn. The tail is the problem: the five slow calls (153–684 ms) all coincided with the TUI or web starting or a poller tick, which means waiting on SQLite's write lock (`busy_timeout` is 5 s). Hooks run synchronously, so a long write transaction elsewhere stalls the developer's session; past 5 s the hook gives up and the state change is lost. A production version should use a short busy timeout and fall back to appending the event to a spool file, or run the write in a detached child. The first invocation of a freshly built binary also took 443 ms (cold page cache).

#### What the TUI shows

No TUI changes. The worktree row's symbol follows the run's status within one refresh:

| Session state | Row | Notes |
|---|---|---|
| Turn running | `⚙` | |
| Permission prompt | `⏸` | |
| Idle as `completed` | `✓` | Identical to a finished headless run and to an ended session |
| Idle as `feedback` | `⏸` | Identical to a permission prompt |
| Idle as `running` | `⚙` | Identical to a running turn |
| Ended | `✓` | |

The worktree detail pane reads `Agent: [running] 0 turns · 41.7s (4 runs) — press x to stop`, and Agent Activity reads "No agent activity" (no `agent_run_events`, no log file). **`x` cancels the run with no confirmation, and `cancel_run` signals `subprocess_pid`, which is the developer's claude process.** That killed the test session (it fired `SessionEnd reason=other`). Every stop path that reaches `cancel_run` must refuse, or at least confirm, for `runtime = 'interactive'`: TUI worktree and repo agent stop, and the web `agent/stop` routes (#3154). `conductor_cancel_run` cancels workflow runs and isn't affected.

#### Orphan reaper

- **A live session is left alone** while its PID is alive. A session sat in `waiting_for_feedback` through several 30 s reaper passes in both pollers.
- **`kill -9` of claude**: no `SessionEnd`. The next reaper pass (about 1 s later, from the web poller) marked the run `failed`: "subprocess exited unexpectedly — agent may have completed but result was not captured". That is correct, with two defects: the failure text overwrote `last_assistant_message` in `result_text`, and the pending feedback request stayed `pending` forever, because the reaper doesn't dismiss requests.
- **`--resume` breaks the PID-recycle guard.** `pid_was_recycled` compares the process start time with the run's `started_at` and reaps if they differ by more than 60 s. A resumed session is a new claude process on an old row, so about 25 s after its first active state the reaper marked the live session `failed` ("subprocess PID recycled"), and `agent_run.failed` went out as a false alarm. The next `Stop` flipped the row back to `completed`. Resetting `started_at` on every `SessionStart` fixed it: the resumed session survived 70 s waiting. A production version needs a separate process-start column (or resets `started_at` and keeps the session start elsewhere). Tracked in #3155.
- **Idle as `completed` is invisible to the reaper** (it only checks active statuses), so a crash while idle leaves a `✓` row that looks finished. That's harmless but slightly misleading.

#### Notifications (`~/.conductor-spike/notify.log`)

- **`feedback.requested` didn't fire at first**: the TUI and web only list pending requests for notification when `[notifications] enabled = true`. `fire_feedback_notification` itself accepts hooks-only configs, but its caller never reaches it. The real config has `enabled = false`, so notification hooks alone never see feedback requests today. That is a bug independent of this RFC.
- With `enabled = true`, a permission prompt produced `feedback.requested` with `PROMPT_PREVIEW=Permission: Bash touch spike-scratch-2.txt`. But **`repo_slug` and `branch` are empty and `RUN_ID` holds the feedback request's id**, so the notification can't say which worktree is waiting. With several sessions open that is the whole point; it needs fixing before notifications are useful here.
- Idle as `completed`: `agent_run.completed` fired once, when the session *started* (a new row first seen as completed). Every later turn end was deduped on `(run_id, agent_completed)`. So it's wrong at the start and silent afterwards.
- Idle as `feedback`: `feedback.requested` fired at every turn end ("Idle: ready") and at session start ("Idle: " with an empty message). That gives the "your turn" ping the tmux plugin provides, at the cost of one notification per turn, using the same kind as a permission prompt.
- The `--resume` false reap above produced `agent_run.failed`.

#### Web Attention Required

`conductor-web` built after `bun install && bun run build` (`frontend/dist` was empty). Attention Required is driven by the worktree's `agent_status == waiting_for_feedback` plus `GET /api/worktrees/{id}/agent/feedback`. Both returned the interactive run's pending request ("Permission: Bash touch …", and "Idle: ready" when idle used feedback), so the section shows interactive sessions with no change. Checked through the API, not in a browser. Its Respond and Dismiss buttons would write a response nothing reads; notify-only would need to hide them for interactive runs.

#### Denied permission and resume

- **Choosing "No" at a permission prompt interrupts the turn.** Claude Code shows "Interrupted · What should Claude do instead?", and **no hook fires at all**: no `PostToolUse`, no `PostToolUseFailure`, no `Stop`, no `PermissionDenied` (registered; per the docs it covers auto-mode denials) and no `StopFailure`. No `idle_prompt` `Notification` arrived in the 2 min 47 s that followed either. The run stays `waiting_for_feedback` until the next `UserPromptSubmit` or `SessionEnd`, so a denial reads as "still blocked". Esc during a turn presumably behaves the same (not tested). One mitigation: treat a pending permission request older than its `Notification(permission_prompt)` plus N seconds with no `PreToolUse` as stale. A cheaper one: show "blocked since" in the UI and accept the false positive. The transcript does record the denial when it happens (a `tool_result` with `toolUseResult: "User rejected tool use"`, then `[Request interrupted by user for tool use]`), so a poller could detect it from `transcript_path`, at the cost of depending on an undocumented format. Tracked in #3156.
- **`claude --resume <id>` keeps the same `session_id`**, and `SessionStart.source` is `resume`, so the upsert reopened the existing row as intended. The resume payload adds fields Spike B didn't see: `context_tokens`, `estimated_cache_write_usd`, `prompt_cache_likely_expired` and `seconds_since_last_response`. These give a cost estimate at resume without reading the transcript.
- `/exit` fires `SessionEnd reason=prompt_input_exit` and no `UserPromptSubmit`. SIGTERM (TUI `x`) fires `SessionEnd reason=other`.

#### Representing idle with a new status (input to migration 087)

A real `idle` status needs a migration: `agent_runs.status` has a CHECK constraint, so SQLite needs the table rebuilt (as in migration 027, about 50 lines, plus its indexes and trigger). Beyond the migration:
- an `AgentRunStatus::Idle` variant, with a decision on whether `is_active()` includes it (the reaper should check idle PIDs; the stop action should not offer itself);
- a TUI symbol;
- web types and the Activity page;
- the reaper's status filter;
- every `match` on the status: about 28 files mention `waiting_for_feedback` and would need checking.

That's a contained change, but not small enough for a spike.
