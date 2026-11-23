# Architecture

Conductor manages repos, git worktrees, tickets and Claude agent runs on one machine, and runs agent workflows against them. Its state lives in a SQLite database in `~/.conductor/`. It ships as a CLI (which is also the MCP server), a TUI, a web UI and a macOS desktop app. Workflows run on [runkon](https://github.com/runkon/runkon), a workflow engine that started inside conductor and is now published separately on crates.io.

## Layers

```
┌─────────────────────────────────────────────────────────────────────┐
│                          USER SURFACES                              │
│                                                                     │
│  conductor (CLI)   conductor-tui   conductor-web   conductor-       │
│   + MCP server      (ratatui)      (axum + React,  desktop (Tauri   │
│                                     embedded)       wraps web)      │
└────────────┬───────────────┬───────────────┬───────────────┬────────┘
             └───────────────┴───────┬───────┴───────────────┘
                                     ▼
        ┌────────────────────────────────────────────────────┐
        │              conductor-core (library)              │
        │                                                    │
        │  Managers:  Repo · Worktree · TicketSyncer ·       │
        │             IssueSource · Agent                    │
        │  Workflow:  coordinator · manager · smoke ·        │
        │             runkon_bridge (conductor → runkon      │
        │             traits)                                │
        │  Notify:    event catalog · hooks · web push       │
        └────────┬─────────────────────────────────┬─────────┘
                 ▼                                 ▼
   ┌──────────────────────────┐    ┌───────────────────────────┐
   │   runkon-* (crates.io)   │    │      Local resources      │
   │                          │    │                           │
   │  runkon-flow             │    │  ~/.conductor/            │
   │  runkon-flow-executors   │    │    conductor.db (SQLite,  │
   │  runkon-runtimes         │    │      WAL)                 │
   │  runkon-anthropic        │    │    config.toml            │
   │  runkon-google           │    │    workspaces/<repo>/     │
   │  runkon-notify           │    │      <worktree>/          │
   └────────────┬─────────────┘    └───────────────────────────┘
                ▼
   ┌──────────────────────────┐
   │   External processes     │
   │                          │
   │  claude (headless CLI)   │
   │  git · gh                │
   │  GitHub / Jira APIs      │
   └──────────────────────────┘
```

Every surface links `conductor-core` directly. There is no daemon, no IPC and no async runtime in core. Processes share state through SQLite in WAL mode, and parallel work runs on threads and subprocesses. All domain structs derive `Serialize`/`Deserialize`, so a daemon could be extracted later without changing them.

## Crates

| Crate | Role |
|---|---|
| `conductor-core` | All domain logic: managers, database and migrations, workflow coordination, notifications |
| `conductor-cli` | clap commands over core. `conductor mcp serve` serves repos, worktrees, tickets and runs as MCP tools and resources over stdio |
| `conductor-tui` | ratatui terminal UI. Blocking work runs off the main thread ([tui-threading.md](tui-threading.md)) |
| `conductor-web` | axum server with an embedded React frontend |
| `conductor-desktop` | Tauri v2 app that wraps `conductor-web` |

The runkon crates, all versioned together (conductor uses `0.10.0-alpha`):

| Crate | Role |
|---|---|
| `runkon-flow` | The engine: `.wf` DSL parser, executor, and the traits a host implements (`ActionExecutor`, `ItemProvider`, `GateResolver`, `RunContext`, `WorkflowPersistence` and others). Includes the SQLite persistence conductor uses (`SqliteWorkflowPersistence`) |
| `runkon-flow-executors` | Vendor-neutral executor pieces: event sinks, env providers, structured-output parsing, agent definition loading |
| `runkon-runtimes` | Spawns, polls and cancels agents as PID-tracked subprocesses |
| `runkon-anthropic` | Claude executors: direct API calls and the headless `claude` CLI |
| `runkon-google` | Gemini executors |
| `runkon-notify` | Notification primitives: event envelope, shell and HTTP hooks, web-push subscriptions |

### Why the engine is a separate project

The workflow engine turned out not to depend on conductor's domain: resumable runs, parallel fan-out, human gates, loops and nested workflows apply to any agent pipeline. Keeping it in conductor-specific code tied it to one host. As runkon, conductor supplies its domain through the engine's traits (`conductor-core/src/workflow/runkon_bridge/`), and model vendors are separate crates that a host opts into. APIs can still break between alpha versions.

## How a workflow run executes

1. A surface starts the run:
   - The TUI calls `spawn_workflow_in_background` (`conductor-tui/src/app/workflow_management.rs`) and runs it on a background thread.
   - MCP `conductor_run_workflow` validates the workflow, then starts the run in its own `conductor` process on the installed binary and waits for it to report the run ID, so the run outlives the MCP connection.
   - The CLI runs it in the foreground.
2. `conductor-core::workflow::coordinator` loads the definition, applies input defaults and hands the run to `runkon-flow`.
3. The engine walks the steps. Agent steps go through `RkActionExecutorAdapter` (`runkon_bridge/action_executor_adapter.rs`), which launches a headless `claude` subprocess. Script steps run `.conductor/scripts/*` with the run's variables in their env. Gates wait for a person.
4. `SqliteWorkflowPersistence` records `workflow_runs` and `workflow_run_steps`. Agent output goes to `agent_runs` and `agent_run_events`.
5. The running process renews a lease on the run. A run whose lease has expired can be resumed by another process (`conductor_resume_run`, or the TUI's stuck-run check).
6. Surfaces show progress by reading the database. Notification hooks fire on run, gate, agent-run and worktree events ([notification-hooks.md](notification-hooks.md)).

## Further reading

- [VISION.md](VISION.md): why conductor exists and where it is going
- [workflow/engine.md](workflow/engine.md): engine design
- [workflow/language.md](workflow/language.md): the `.wf` language
- [diagrams/](diagrams/): Mermaid diagrams of the schema, data flow and state machines
- [development.md](development.md): building and testing
