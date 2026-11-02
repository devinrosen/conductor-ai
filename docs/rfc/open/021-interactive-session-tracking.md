# RFC 021: Interactive Session Tracking

**Status:** Pre-RFC Research
**Created:** 2026-11-02

---

## Problem

Conductor tracks the headless agents it spawns. It knows nothing about the Claude Code sessions a developer runs by hand in a conductor worktree, which is how judgement-heavy tickets get worked. With three or four of those open at once, the developer is back to the overhead conductor exists to remove: remembering which terminal holds which ticket, which session is blocked on a permission prompt, and which finished its turn and is waiting for them.

A stopgap exists outside conductor: a personal Claude Code plugin (`devinrosen/claude-plugin-work`) that launches each ticket's session in a named tmux window, colours the window from Claude Code hooks (yellow on a permission prompt, green when a turn ends) and reports status by reading the panes. It duplicates conductor's worktree and ticket tracking, and none of it reaches the TUI, the web app or a phone.

## Proposal (sketch)

Track an interactive session as an **`agent_runs` row with `runtime = 'interactive'`**, keyed by Claude Code's `session_id`, and let Claude Code hooks report its state through a **CLI command** that writes the database. The existing pollers, notification hooks, TUI symbols and the web app's Attention Required section then work with little or no change. This follows TASTE.md's "prefer composition over parallel systems" and DIRECTION.md's "library-first, no daemon".

| Claude Code hook | Run state |
|---|---|
| `SessionStart` | Create the run for this `session_id`, or reopen it on `--resume` |
| `UserPromptSubmit` | `running` |
| `PermissionRequest` | `waiting_for_feedback`, plus a `feedback_requests` row naming the tool and its input |
| `PostToolUse` | back to `running` (the prompt was answered) |
| `Stop` | idle: the turn ended and it's the developer's move |
| `SessionEnd` | `completed` |

Conductor would also own launching and resuming sessions in terminals (tmux from the TUI, plus an MCP tool), using the stored `session_id` for `claude --resume`.

First pass is **notify-only**: conductor tells the developer a session is waiting (TUI, web, push); they answer it in the terminal. Answering from a phone (TASTE.md's phone-browser requirement) is a later phase; `PermissionRequest` hooks can return an allow/deny decision, which could reuse the feedback mechanism.

## Spike A: launch latency (2026-11-02)

Conductor's old tmux mode was dropped partly because launching felt slow. Measured on Claude Code 2.1.316, from `tmux new-window ... claude` to the input prompt being drawn, with the user's full plugin and MCP server set:

| Launch | Runs | First paint | Prompt ready |
|---|---|---|---|
| Fresh `claude` | 4 | 0.36–0.37 s | 0.60–0.61 s |
| `claude --resume` of a 10 MB transcript | 3 | 0.60–0.79 s | 1.72–1.75 s |

`SessionStart` hooks fire about 0.64 s after the window opens. Launch latency is no longer a reason to keep conductor out of terminals. Not measured: MCP servers may still be connecting after the prompt appears.

## Spike B: hook payloads (2026-11-02)

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
5. **No hook reports cost or tokens.** Per-turn or per-session cost means reading `transcript_path`. (Spike C: the transcript's `cost-state` entries carry session totals; see open question 1.)
6. **The hook process's parent is the `claude` process**, so a hook can record its PID for the orphan reaper (same PID plus start-time guard as headless runs).
7. **Closing the tmux window still fires `SessionEnd`** with `reason=other`. A crash or SIGKILL presumably doesn't (untested), which the reaper covers.
8. **`TMUX_PANE` is in the hook's environment**, so conductor can record which tmux pane to focus.
9. Ignore `SubagentStop` for session state: one fired after `Stop` for an internal agent.

Not yet observed: a denied permission (probably no `PostToolUse`, so `Stop` and `UserPromptSubmit` must also clear waiting), `SessionStart.source` on `--resume` (expected `resume`), compaction.

## Spike C: hook → database → TUI (2026-11-02)

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

### Hook cost

Release build, Apple Silicon, 434 MB database. The hook path skips `check_prerequisites()` (three subprocess spawns) and config loading. It opens the database with the compat migration check and runs 1–3 statements.

| Measurement | p50 | p90 | max |
|---|---|---|---|
| Wall time per invocation, 100 × `PostToolUse` from a script | 12.7 ms | 15.9 ms | 26 ms |
| Wall time, 30 × `PermissionRequest` (insert) | 12.6 ms | 14.3 ms | 15 ms |
| In-process time (open + write), 37 hooks from live sessions | 6.0 ms | 153 ms | 684 ms |
| Spawning `/usr/bin/true`, for scale | 2.2 ms | | |
| `conductor repo list`, the normal CLI path with prerequisite checks | ~40 ms | | |

The hot path costs a few milliseconds per tool call, invisible against a model turn. The tail is the problem: the five slow calls (153–684 ms) all coincided with the TUI or web starting or a poller tick, which means waiting on SQLite's write lock (`busy_timeout` is 5 s). Hooks run synchronously, so a long write transaction elsewhere stalls the developer's session; past 5 s the hook gives up and the state change is lost. A production version should use a short busy timeout and fall back to appending the event to a spool file, or run the write in a detached child. The first invocation of a freshly built binary also took 443 ms (cold page cache).

### What the TUI shows

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

### Orphan reaper

- **A live session is left alone** while its PID is alive. A session sat in `waiting_for_feedback` through several 30 s reaper passes in both pollers.
- **`kill -9` of claude**: no `SessionEnd`. The next reaper pass (about 1 s later, from the web poller) marked the run `failed`: "subprocess exited unexpectedly — agent may have completed but result was not captured". That is correct, with two defects: the failure text overwrote `last_assistant_message` in `result_text`, and the pending feedback request stayed `pending` forever, because the reaper doesn't dismiss requests.
- **`--resume` breaks the PID-recycle guard.** `pid_was_recycled` compares the process start time with the run's `started_at` and reaps if they differ by more than 60 s. A resumed session is a new claude process on an old row, so about 25 s after its first active state the reaper marked the live session `failed` ("subprocess PID recycled"), and `agent_run.failed` went out as a false alarm. The next `Stop` flipped the row back to `completed`. Resetting `started_at` on every `SessionStart` fixed it: the resumed session survived 70 s waiting. A production version needs a separate process-start column (or resets `started_at` and keeps the session start elsewhere). Tracked in #3155.
- **Idle as `completed` is invisible to the reaper** (it only checks active statuses), so a crash while idle leaves a `✓` row that looks finished. That's harmless but slightly misleading.

### Notifications (`~/.conductor-spike/notify.log`)

- **`feedback.requested` didn't fire at first**: the TUI and web only list pending requests for notification when `[notifications] enabled = true`. `fire_feedback_notification` itself accepts hooks-only configs, but its caller never reaches it. The real config has `enabled = false`, so notification hooks alone never see feedback requests today. That is a bug independent of this RFC.
- With `enabled = true`, a permission prompt produced `feedback.requested` with `PROMPT_PREVIEW=Permission: Bash touch spike-scratch-2.txt`. But **`repo_slug` and `branch` are empty and `RUN_ID` holds the feedback request's id**, so the notification can't say which worktree is waiting. With several sessions open that is the whole point; it needs fixing before notifications are useful here.
- Idle as `completed`: `agent_run.completed` fired once, when the session *started* (a new row first seen as completed). Every later turn end was deduped on `(run_id, agent_completed)`. So it's wrong at the start and silent afterwards.
- Idle as `feedback`: `feedback.requested` fired at every turn end ("Idle: ready") and at session start ("Idle: " with an empty message). That gives the "your turn" ping the tmux plugin provides, at the cost of one notification per turn, using the same kind as a permission prompt.
- The `--resume` false reap above produced `agent_run.failed`.

### Web Attention Required

`conductor-web` built after `bun install && bun run build` (`frontend/dist` was empty). Attention Required is driven by the worktree's `agent_status == waiting_for_feedback` plus `GET /api/worktrees/{id}/agent/feedback`. Both returned the interactive run's pending request ("Permission: Bash touch …", and "Idle: ready" when idle used feedback), so the section shows interactive sessions with no change. Checked through the API, not in a browser. Its Respond and Dismiss buttons would write a response nothing reads; notify-only would need to hide them for interactive runs.

### Denied permission and resume

- **Choosing "No" at a permission prompt interrupts the turn.** Claude Code shows "Interrupted · What should Claude do instead?", and **no hook fires at all**: no `PostToolUse`, no `PostToolUseFailure`, no `Stop`, no `PermissionDenied` (registered; per the docs it covers auto-mode denials) and no `StopFailure`. No `idle_prompt` `Notification` arrived in the 2 min 47 s that followed either. The run stays `waiting_for_feedback` until the next `UserPromptSubmit` or `SessionEnd`, so a denial reads as "still blocked". Esc during a turn presumably behaves the same (not tested). One mitigation: treat a pending permission request older than its `Notification(permission_prompt)` plus N seconds with no `PreToolUse` as stale. A cheaper one: show "blocked since" in the UI and accept the false positive. The transcript does record the denial when it happens (a `tool_result` with `toolUseResult: "User rejected tool use"`, then `[Request interrupted by user for tool use]`), so a poller could detect it from `transcript_path`, at the cost of depending on an undocumented format. Tracked in #3156.
- **`claude --resume <id>` keeps the same `session_id`**, and `SessionStart.source` is `resume`, so the upsert reopened the existing row as intended. The resume payload adds fields Spike B didn't see: `context_tokens`, `estimated_cache_write_usd`, `prompt_cache_likely_expired` and `seconds_since_last_response`. These give a cost estimate at resume without reading the transcript.
- `/exit` fires `SessionEnd reason=prompt_input_exit` and no `UserPromptSubmit`. SIGTERM (TUI `x`) fires `SessionEnd reason=other`.

### Representing idle with a new status

A real `idle` status needs a migration: `agent_runs.status` has a CHECK constraint, so SQLite needs the table rebuilt (as in migration 027, about 50 lines, plus its indexes and trigger). Beyond the migration:
- an `AgentRunStatus::Idle` variant, with a decision on whether `is_active()` includes it (the reaper should check idle PIDs; the stop action should not offer itself);
- a TUI symbol;
- web types and the Activity page;
- the reaper's status filter;
- every `match` on the status: about 28 files mention `waiting_for_feedback` and would need checking.

That's a contained change, but not small enough for a spike.

## Open questions

1. **One run per session, or one per turn?** **Decided (2026-11-02): one run per session.** The upsert on `session_id` survives `--resume` (same id), the worktree row shows one current state, and per-turn rows would multiply the `started_at`/PID problems (#3155). The `agent_run.completed` dedup is keyed on `run_id` either way.

   **Metrics.** No hook reports tokens or cost, but the transcript at `transcript_path` does. Checked against the Spike C session's transcript, it has two sources:

   - **`cost-state` entries**: Claude Code's own cumulative session totals. They carry `totalCostUSD`, `totalAPIDuration`, `totalToolDuration`, lines added and removed, and per-model `modelUsage` (input, output, thinking, cache-read and cache-creation tokens). The totals accumulate across resumes (one `startTime`). That session had three entries and three exits, so they are probably written at session end; not confirmed. A `kill -9` probably leaves no final entry.
   - **Per-message `usage`** on assistant entries (tokens and model per API request). Entries carry no `promptId`, but their `parentUuid` chain reaches the user entry that does, which allows per-turn grouping. Entries repeat per content block, so deduplicate on `message.id`: 9 entries were 8 messages. The sums don't match `cost-state` (input 16 vs 2,252 tokens), probably because `cost-state` also counts Claude Code's side calls (titles, classifiers, internal subagents). Reconcile before trusting per-turn figures.

   **Where they go:**
   - **Session totals** go on the run row's existing columns (`cost_usd`, the token columns, `duration_ms`, `num_turns`), from the latest `cost-state` entry. Read them at `Stop` or `SessionEnd`, but in a poller or detached child, not in the blocking hook (see open question 7). Fall back to summing message usage when no final entry exists.
   - **Per-turn detail** (prompt, last message, tokens, duration) goes into `agent_run_events` keyed by `prompt_id` (RFC 020).
   - **On resume**, `SessionStart` also carries `context_tokens` and `estimated_cache_write_usd`: an estimate of what continuing will cost.
   - **Caveats:** both sources are Claude Code's internal transcript format, which is undocumented and has changed before (`costUSD` per entry is gone). Where the metric columns live may move with the metric-column extraction tracked in runkon/runkon#6.
2. **How is "idle" represented?** **Decided (2026-11-02): a new `idle` status** (#3157), active for the reaper but not stoppable, with its own TUI symbol and kept out of Attention Required. **Idle notifies as its own `session.idle` event** (#3159), separate from `feedback.requested`. It fires from the pollers once per finished turn (not on `SessionStart`), deduplicated per turn, and names the worktree. It fires after a **short configurable delay** (`[notifications.session_idle] delay_secs`, default 30 s), measured from an `idle_since` timestamp, so a developer who types straight away isn't pinged. It fires even when background tasks are still running, and carries their count. Web Push sends only `feedback.requested` and gate-waiting by default; `session.idle` push is opt-in (`[notifications.session_idle] web_push = true`). Hooks filter with their own `on`; no further toggle. Web Push for feedback and gates is itself broken for database-written state today (#3162).
3. **Hook installation.** **Decided (2026-11-02): ship the hooks in conductor's Claude Code plugin** (#3161). A wrapper exits silently when the hook doesn't apply:
   - headless sessions: hooks see `CLAUDE_CODE_SESSION_ATTENDED=0` and `CLAUDE_CODE_ENTRYPOINT=sdk-cli` under `claude -p`, verified. Conductor's own workflow agents carry no `CONDUCTOR_RUN_ID`, so this check is what keeps them out;
   - `conductor` not on `PATH`, or a binary too old to have the subcommand;
   - `cwd` outside a conductor worktree.

   The hook records `CLAUDE_PID` rather than its parent PID. The plugin is **new and lives in this repo**: the repo is its own marketplace (`.claude-plugin/marketplace.json`, plugin in `plugin/`), and the plugin is versioned with the conductor release. The first version ships hooks only; the MCP server and skills stay where they are for now. Sessions outside an active worktree, including a repo's main checkout, are **ignored**; there is no repo-scoped fallback.
4. **Terminal ownership.** **Decided (2026-11-02): conductor launches and resumes sessions** (#3158). One core function serves a TUI key, `conductor session open` and an MCP tool. Each launches into a tmux window, using `--resume` with the worktree's newest `session_id` when there is one, and focuses the pane instead if the session is live (the hook records `TMUX_PANE`). Details decided in #3158:
   - tmux session `work` by default, configurable, and created detached if missing;
   - windows named by ticket key, falling back to the worktree slug;
   - a generated opener for fresh sessions with a linked ticket, and no prompt on resume;
   - outside tmux, the attach command is shown and copied;
   - the `work` plugin shrinks to its manager skill and calls conductor.

   Taking over a headless run's session is a separate follow-up (#3163).
5. **Stop and respond actions must know about interactive runs** (#3154). `cancel_run` kills the developer's claude. The feedback Respond button and `conductor_submit_agent_feedback` write a response nothing reads, until the phone-answer phase uses `PermissionRequest`'s decision output.
6. **Denied permissions leave no hook trace** (#3156). No hook fires after "No", though the transcript records it. Accept the stale `waiting_for_feedback` until the next prompt, add an age-based heuristic, or ask upstream for an interrupt or deny hook?
7. **Lock contention.** **Decided (2026-11-02): never block on SQLite** (#3160). The hook writes directly with a short busy timeout, and on `SQLITE_BUSY` appends the event to a per-session spool file. Once a session has a spool, events keep going to it so they stay in order. The next hook and the TUI and web pollers drain it, applying events in order with their own timestamps.
8. **Notification payloads** (#3152, #3153) need the worktree: `feedback.requested` carries no repo or branch, and its `run_id` field is the request id. The `notifications.enabled` gate on the feedback query should also be fixed so hooks-only configs receive feedback events.
