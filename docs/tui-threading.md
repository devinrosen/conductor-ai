# TUI Threading Model

This document diagrams the conductor-tui threading architecture: how the main thread,
background threads, and channels interact to keep the UI responsive while all blocking
work happens off-thread.

---

## Diagram 1 — Thread topology

```mermaid
graph LR
    subgraph Main["Main Thread"]
        direction TB
        E1["crossterm input drain\n(high priority)"]
        E2["background action drain\n(low priority)"]
        UP["app.update(action)"]
        RD["ratatui render\n(dirty-flag gated)"]
        E1 --> UP --> RD
        E2 --> UP
    end

    subgraph BS["BackgroundSender (cloneable wrapper)"]
        direction TB
        ATX["mpsc::Sender&lt;Action&gt;\n(action_tx)"]
        WTX["mpsc::Sender&lt;Wake&gt;\n(wake_tx)"]
    end

    subgraph BG["Background Threads"]
        direction TB
        T0["crossterm input reader\npoll every 10ms → Wake::Input"]
        T1["tick timer\nevery 200ms → Action::Tick"]
        T2["spawn_db_poller\nevery 5s → Action::DataRefreshed"]
        T3["spawn_ticket_sync\nevery config×60s → TicketSyncComplete/Failed/Done\n(staleness guard: skip if synced within 300s)"]
        T4["spawn_workflow_in_background\non-demand, shutdown via Arc&lt;AtomicBool&gt;\n→ WorkflowStarted / WorkflowCompleted / BackgroundError"]
        T5["spawn_pr_fetch_once\none-shot, PrFetchGuard AtomicBool dedup\n→ Action::PrsRefreshed"]
        T6["spawn_main_health_check\none-shot on worktree create\n→ Action::MainHealthCheckComplete"]
        T7["anonymous spawn (handle_delete)\none-shot, has_merged_pr() check\n→ Action::DeleteWorktreeReady"]
    end

    subgraph Shared["Arc-shared mutable state"]
        S1["Arc&lt;Mutex&lt;Option&lt;String&gt;&gt;&gt;\nselected_worktree_id"]
        S2["Arc&lt;Mutex&lt;Option&lt;String&gt;&gt;&gt;\nselected_repo_id"]
    end

    T0 -- "key event → input_rx\n+ Wake::Input → wake_rx" --> Main
    T1 -- "Action::Tick via BackgroundSender" --> BS
    T2 -- "Action::DataRefreshed via BackgroundSender" --> BS
    T3 -- "TicketSync* via BackgroundSender" --> BS
    T4 -- "Workflow* via BackgroundSender" --> BS
    T5 -- "PrsRefreshed via BackgroundSender" --> BS
    T6 -- "MainHealthCheckComplete via BackgroundSender" --> BS
    T7 -- "DeleteWorktreeReady via BackgroundSender" --> BS
    BS -- "action → bg_rx\n+ Wake::Background → wake_rx" --> Main
    Main -- "read" --> Shared
    T2 -- "read" --> Shared
```

**Drain order:** `input_rx` is always drained before `bg_rx`. The main loop blocks on
`wake_rx` (blocking nothing), then processes input first, background second.
Rendering only happens when the `dirty` flag is set by `app.update()`.

---

## Diagram 2 — DB poll throttle tiers

The `spawn_db_poller` thread runs a full DB snapshot every **5 seconds**, but several
sub-operations inside `poll_data()` are further throttled with static `AtomicI64`
timestamps to avoid hammering local and remote resources on every tick.

```mermaid
graph TD
    A["poll_data() called\nevery 5s"] --> B["Full DB snapshot\n→ Action::DataRefreshed\nevery tick"]

    A --> C{"LAST_REAP\n≥ 30s ago?"}
    C -- yes --> D["reap_orphaned_runs()\ndismiss_expired_feedback_requests()\nreap_stale_worktrees()\ncleanup_merged_worktrees()\nrecover_stuck_steps()\nreap_orphaned_workflow_runs()\nreap_finalization_stuck_workflow_runs()"]
    C -- no --> E["skip"]

    D --> F{"LAST_DANGLING_REAP\n≥ 300s ago?"}
    F -- yes --> G["reap_dangling_all()\n(gh pr list subprocess\nper candidate)"]
    F -- no --> H["skip"]

    A --> I{"LAST_REFRESH\n≥ 60s ago?"}
    I -- yes --> J["refresh_last_commit_all()\nper repo (git subprocess)"]
    I -- no --> K["skip"]
```

| Constant | Interval | What runs |
|---|---|---|
| _(outer tick)_ | 5s | Full DB snapshot → `DataRefreshed` |
| `LAST_REAP` | 30s | Orphan reaping, stale worktree cleanup, stuck step recovery |
| `LAST_REFRESH` | 60s | `last_commit_at` refresh via git subprocess |
| `LAST_DANGLING_REAP` | 300s | `reap_dangling_all()` — `gh pr list` per candidate |
| `TICKET_SYNC_STALE_SECS` | 300s | Ticket sync staleness guard — skip if recently synced |

---

## Diagram 3 — Canonical blocking-op handoff

This sequence matches the required pattern from CLAUDE.md's TUI Threading Rule.
Every blocking call (git, subprocess, network, large file I/O) follows this flow.

```mermaid
sequenceDiagram
    participant U as User
    participant MT as Main Thread
    participant BS as BackgroundSender
    participant BT as Spawned Thread
    participant DB as SQLite / OS

    U->>MT: key press triggers action
    MT->>MT: clone bg_tx, capture params
    MT->>MT: set Modal::Progress (non-dismissable)
    MT->>BT: std::thread::spawn(move || { ... })
    BT->>DB: open fresh DB connection
    BT->>DB: run blocking op (git / network / file I/O)
    DB-->>BT: result
    BT->>BS: bg_tx.send(Action::*Complete { result })
    BS->>MT: action pushed to bg_rx
    BS->>MT: Wake::Background sent to wake_rx (unblocks wait())
    MT->>MT: drain_background() receives Action::*Complete
    MT->>MT: clear Modal::Progress
    alt success
        MT->>MT: update state / show status_message
    else error
        MT->>MT: set Modal::Error { message }
    end
    MT->>MT: dirty = true → ratatui render
```

---

## Reference implementations

These four call sites already follow the canonical pattern above.

| Implementation | File | Blocking op | Action sent |
|---|---|---|---|
| PR merged check | `crud_operations.rs:442` | `has_merged_pr()` — `gh pr list` subprocess | `Action::DeleteWorktreeReady` |
| Main health check | `crud_operations.rs:564` | `check_main_health()` — git fetch/status | `Action::MainHealthCheckComplete` |
| Workflow execution | `workflow_management.rs:1175` | `execute_workflow_standalone()` — conductor subprocess | `WorkflowStarted` / `WorkflowCompleted` |
| PR fetch | `background.rs:1359` | `list_open_prs()` — `gh pr list` subprocess | `Action::PrsRefreshed` |

---

## Anti-patterns

> **Never do any of the following on the main thread:**
>
> - Call `std::process::Command` (git, gh, conductor, any subprocess)
> - Read large files in the render path (agent logs, workflow output)
> - Run slow or write-heavy DB queries while holding render budget
> - Use `.unwrap()` panics inside spawned threads — send an error result back instead;
>   a panic in a spawned thread cannot be caught on the main thread and will crash the TUI
