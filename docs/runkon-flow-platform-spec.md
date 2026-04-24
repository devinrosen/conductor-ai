# Workflow Engine Platform Spec

**Status:** Draft  
**Date:** 2026-04-19

---

## Vision

A standalone Rust library — working title `runkon-flow` — that provides a general-purpose,
resumable workflow engine. It has no opinions about what "actions" are, what data collections
exist, or what gates mean. Those are supplied by the host application (a *harness*).

Conductor becomes the first harness. A communication routing harness (email, Slack, etc.)
becomes the second. The engine is published to crates.io so any project can build a harness.

```
┌─────────────────────────────────────────────────────────┐
│                       runkon-flow                       │
│   DSL parser · control flow · resumability · DB schema  │
│   if/while/parallel/gate/foreach/always/script          │
└────────────┬──────────────────────┬─────────────────────┘
             │                      │
    ┌────────▼────────┐    ┌────────▼──────────────┐
    │ conductor-core  │    │ comm-harness (new)     │
    │ (first harness) │    │ (second harness)       │
    │                 │    │                        │
    │ ActionExecutor  │    │ ActionExecutor         │
    │  → Claude subp. │    │  → email/Slack/HTTP    │
    │ ItemProvider    │    │ ItemProvider           │
    │  → tickets/     │    │  → inbox/threads/      │
    │    repos        │    │    channels            │
    │ GateResolver    │    │ GateResolver           │
    │  → PR approval/ │    │  → Slack reaction/     │
    │    PR checks    │    │    email reply         │
    │ TriggerSource   │    │ TriggerSource          │
    │  → manual/PR    │    │  → email received/     │
    │                 │    │    webhook/cron        │
    └─────────────────┘    └────────────────────────┘
```

---

## Why Extract

The conductor workflow engine is production-tested: resumable state machine, parallel
fan-out, gate semantics, loop control, nested workflow composition. All of that logic is
domain-agnostic. But four coupling points make it impossible to use outside conductor:

1. `ExecutionState` carries 40+ conductor-specific fields (worktree, ticket, repo, PR)
2. `ENGINE_INJECTED_KEYS` hardcodes 14 conductor-specific variable names
3. `ForeachOver` is a closed enum: `Tickets | Repos | WorkflowRuns`
4. Gate executor calls `gh` CLI directly for `pr_approval` and `pr_checks`
5. `call` steps are hardwired to spawn Claude agents via headless subprocess (PID-based)

The DSL parser and AST are already clean. The extraction surface is the execution layer only.

---

## Core Abstractions

Six traits define the contract between the engine and a harness. A harness implements the
ones it needs and registers them at engine startup.

---

### 1. `ActionExecutor`

What a `call` step does. This is the most important trait — it replaces the hardcoded
Claude subprocess invocation.

```rust
pub trait ActionExecutor: Send + Sync {
    /// The name used in the DSL: `call <name>`. For executors registered via
    /// `.action(...)` this is the dispatch key. For fallback executors registered
    /// via `.action_fallback(...)`, this value is diagnostic only (conventionally
    /// `"__fallback__"`) — the builder method determines registration, not this
    /// return value.
    fn name(&self) -> &str;

    /// Execute the action. The engine calls this when a `call` node is reached.
    /// The original DSL-level call name is available as `params.name`, which
    /// fallback executors use to dispatch internally (e.g., conductor's
    /// `ClaudeAgentExecutor` uses it to look up the agent `.md` file).
    /// Returns structured output that feeds the marker/context system.
    fn execute(
        &self,
        ectx: &ExecutionContext,
        params: &ActionParams,
    ) -> Result<ActionOutput, EngineError>;

    /// Advisory cancel of an in-flight execution. Called when the run is
    /// cancelled (user request, parallel fail_fast, timeout, parent cancel,
    /// engine shutdown). The engine fires this and moves on — it does not
    /// wait for `cancel()` to return. Executors use this to preempt external
    /// work: conductor's `ClaudeAgentExecutor` kills the subprocess (via PID); an
    /// HTTP-based executor aborts the in-flight request.
    ///
    /// Well-behaved executors also check `ectx.cancellation.is_cancelled()`
    /// from inside `execute()` — cooperative cancel is the primary path.
    /// `cancel()` is the escalation for external work that can't observe
    /// the cooperative token.
    fn cancel(&self, execution_id: &str) -> Result<(), EngineError> {
        let _ = execution_id;
        Ok(())
    }
}

pub struct ActionParams {
    pub name: String,
    pub inputs: HashMap<String, String>,   // resolved {{variable}} substitutions
    pub retries_remaining: u32,
    pub prior_context: Option<String>,
    pub snippets: Vec<String>,             // contents of `with = [...]` snippets
    pub dry_run: bool,
}

pub struct ActionOutput {
    pub markers: Vec<String>,             // drives if/while conditions
    pub context: Option<String>,          // passed as {{prior_context}} to next step
    pub metadata: HashMap<String, String>,// arbitrary key-value for harness use
}
```

**Conductor's implementation:** `ClaudeAgentExecutor` — resolves the agent `.md` file,
builds the prompt, spawns a headless subprocess (PID-based), polls for `CONDUCTOR_OUTPUT`,
and maps the result to `ActionOutput`.

**Communication harness implementations:** `SendEmailExecutor`, `PostSlackExecutor`,
`CreateJiraTicketExecutor`, `HttpRequestExecutor`, etc. Each one is a small struct that
calls the relevant API and maps the response to markers and context.

**Registration paths (per Open Q #1):**

The builder exposes two ways to register an `ActionExecutor`:

- `.action(Box<dyn ActionExecutor>)` — registers by the executor's `name()`.
  Dispatched when a `call <name>` step matches exactly. Comm-harness pattern.
- `.action_fallback(Box<dyn ActionExecutor>)` — registers as the catch-all.
  Dispatched when no named executor matches the `call` name. Conductor's pattern:
  one `ClaudeAgentExecutor` serves every `call <name>` by resolving the agent
  `.md` file dynamically from `params.name`.

Named executors take precedence over the fallback — so a harness can mix both
(e.g., register a `SpecialAgentExecutor` by name AND `ClaudeAgentExecutor` as
fallback for everything else). At most one fallback per engine; a second
`.action_fallback(...)` call is an error at `build()` time. When no match is
found and no fallback is configured, the engine returns
`"no registered ActionExecutor for 'name' and no fallback configured"` at
dispatch time.

---

### 2. `ItemProvider`

What `foreach over <name>` fans out over. Replaces the closed `ForeachOver` enum.

```rust
pub trait ItemProvider: Send + Sync {
    /// Name used in the DSL: `foreach over <name>`
    fn name(&self) -> &str;

    /// Collect items to fan out over. Called once at foreach step start.
    /// Providers that do slow I/O (remote fetch, IMAP scan) should check
    /// `ectx.cancellation.is_cancelled()` during collection.
    fn items(
        &self,
        ectx: &ExecutionContext,
        scope: &HashMap<String, String>,
        filter: &HashMap<String, String>,
    ) -> Result<Vec<FanOutItem>, EngineError>;

    /// Optional: return dependency edges for ordered dispatch.
    /// (item_id, blocks_item_id) pairs. Engine uses these for `ordered = true`.
    fn dependencies(&self, items: &[FanOutItem]) -> Vec<(String, String)> {
        vec![]
    }
}

pub struct FanOutItem {
    pub id: String,
    pub label: String,
    pub context: HashMap<String, String>,  // injected as {{item.*}} in child workflows
}
```

**Conductor's implementations:** `TicketsProvider`, `ReposProvider`, `WorkflowRunsProvider`,
`WorktreesProvider`.

**Communication harness implementations:** `InboxProvider` (email threads needing triage),
`SlackChannelProvider` (messages from a channel), `WebhookQueueProvider`.

---

### 3. `GateResolver`

What gates wait for. Replaces hardcoded GitHub polling.

```rust
pub trait GateResolver: Send + Sync {
    /// Gate type name used in the DSL: `gate <type> { ... }`
    fn gate_type(&self) -> &str;

    /// Poll once. The engine calls this on each tick for `running` gate steps.
    ///   Ok(GatePoll::Approved(feedback)) — gate passed; optional feedback string
    ///   Ok(GatePoll::Rejected(reason))   — gate failed
    ///   Ok(GatePoll::Pending)            — still waiting
    ///   Err(...)                          — unrecoverable error
    fn poll(
        &self,
        run_id: &str,
        params: &GateParams,
        ectx: &ExecutionContext,
    ) -> Result<GatePoll, EngineError>;
}

pub enum GatePoll {
    Approved(Option<String>),  // optional feedback text
    Rejected(String),          // rejection reason
    Pending,
}

pub struct GateParams {
    pub prompt: Option<String>,
    pub timeout: Option<Duration>,
    pub on_timeout: OnTimeout,
    pub options: HashMap<String, String>,
}
```

**Conductor's implementations:** `PrApprovalGateResolver`, `PrChecksGateResolver`,
`HumanApprovalGateResolver` (polls DB for CLI/TUI/web approval action).

**Communication harness implementations:** `SlackReactionGateResolver` (waits for ✅ or ❌
on a message), `EmailReplyGateResolver` (waits for a reply with a keyword), `WebhookGateResolver`.

---

### 4. `TriggerSource`

What causes a workflow to start. Currently stubs in conductor; first-class in the extracted
engine.

```rust
pub trait TriggerSource: Send + Sync {
    /// Trigger type name used in the DSL: `trigger = "<name>"`
    fn name(&self) -> &str;

    /// Check for pending trigger events. Returns zero or more inputs maps,
    /// each causing a new workflow run to start.
    /// The engine calls this on each poll tick for registered workflows.
    fn poll(&self, ctx: &TriggerContext) -> Result<Vec<TriggerEvent>, EngineError>;

    /// Mark a trigger event as consumed so it is not re-fired.
    fn ack(&self, event_id: &str) -> Result<(), EngineError>;
}

pub struct TriggerEvent {
    pub event_id: String,                  // for ack()
    pub inputs: HashMap<String, String>,   // passed to the workflow run
}

pub struct TriggerContext {
    pub workflow_name: String,
    pub workflow_def: WorkflowDef,
}
```

**Conductor's implementations:** `ManualTriggerSource` (always returns empty — runs are
started explicitly by CLI/TUI/web), `PrTriggerSource` (polls for new PRs matching a pattern).

**Communication harness implementations:** `ImapTriggerSource` (polls for new email),
`SlackEventTriggerSource` (reads Slack events API), `WebhookTriggerSource` (processes
inbound HTTP events), `CronTriggerSource`.

---

### 5. `RunContext`

Injected variables available to every step. Replaces the 40+ hardcoded fields in
`ExecutionState` and the `ENGINE_INJECTED_KEYS` constant.

```rust
pub trait RunContext: Send + Sync {
    /// Variable key-value pairs injected into every step's template substitution.
    /// These are reserved — the engine rejects workflows that define inputs with
    /// the same names.
    fn injected_variables(&self) -> HashMap<String, String>;

    /// Working directory for `script` steps and spawned agent processes.
    /// May be different per run (e.g., a git worktree path in conductor, a temp
    /// dir in a comm harness). Owned return for consistency with
    /// `injected_variables()` and `script_env()`; per-call allocation is
    /// negligible at this call frequency.
    ///
    /// *Template substitution:* when `{{working_dir}}` appears in a template,
    /// the engine renders the returned `PathBuf` via `.to_string_lossy()`.
    /// Non-UTF-8 paths render with replacement characters; conductor's paths
    /// are always UTF-8 so this is lossless in practice.
    fn working_dir(&self) -> PathBuf;

    /// Environment variables merged into `script` step command env.
    /// Harness composes `PATH` itself (e.g., prepends plugin dirs onto the
    /// inherited `PATH`). Default empty for harnesses that don't need script env.
    ///
    /// *Planned migration:* this accessor will move to a dedicated
    /// `ScriptEnvProvider` trait before `runkon-flow 0.1.0-alpha` is published to
    /// crates.io. See Open Question #3.
    fn script_env(&self) -> HashMap<String, String> { HashMap::new() }
}
```

**Conductor's implementation:** `WorktreeRunContext` — injects `ticket_id`, `repo_path`,
`worktree_id`, `workflow_run_id`, etc.

**Communication harness implementation:** `MessageRunContext` — injects `message_id`,
`sender_email`, `subject`, `received_at`, `thread_id`, etc.

---

### 6. `WorkflowPersistence`

Storage backend. Defaults to SQLite; swappable for tests or alternative backends.

```rust
pub trait WorkflowPersistence: Send + Sync {
    fn create_run(&self, def_snapshot: &str, inputs: &HashMap<String, String>, dry_run: bool) -> Result<String, EngineError>;
    fn get_run(&self, run_id: &str) -> Result<Option<RunRecord>, EngineError>;
    fn list_active_runs(&self) -> Result<Vec<RunRecord>, EngineError>;
    fn update_run_status(&self, run_id: &str, status: RunStatus, error: Option<&str>) -> Result<(), EngineError>;
    fn insert_step(&self, run_id: &str, step: NewStep) -> Result<String, EngineError>;
    fn update_step(&self, step_id: &str, update: StepUpdate) -> Result<(), EngineError>;
    fn get_steps(&self, run_id: &str) -> Result<Vec<StepRecord>, EngineError>;
    fn insert_fan_out_item(&self, step_id: &str, item: &FanOutItem) -> Result<String, EngineError>;
    fn update_fan_out_item(&self, item_id: &str, status: ItemStatus) -> Result<(), EngineError>;
    fn get_fan_out_items(&self, step_id: &str) -> Result<Vec<FanOutItemRecord>, EngineError>;
}
```

**Conductor's implementation:** `SqliteWorkflowPersistence` — existing schema
(`workflow_runs`, `workflow_run_steps`, `workflow_run_step_fan_out_items`).

**Test implementation:** `InMemoryWorkflowPersistence` — ships with `runkon-flow` for use in
harness unit tests.

---

## Engine Initialization

```rust
let engine = FlowEngine::builder()
    .persistence(Box::new(SqliteWorkflowPersistence::new(&db)))
    .run_context(Box::new(WorktreeRunContext::new(&worktree)))
    .action_fallback(Box::new(ClaudeAgentExecutor::new(&config)))
    .item_provider(Box::new(TicketsProvider::new(&db)))
    .item_provider(Box::new(ReposProvider::new(&db)))
    .item_provider(Box::new(WorkflowRunsProvider::new(&db)))
    .item_provider(Box::new(WorktreesProvider::new(&db)))
    .gate_resolver(Box::new(PrApprovalGateResolver::new()))
    .gate_resolver(Box::new(PrChecksGateResolver::new()))
    .gate_resolver(Box::new(HumanApprovalGateResolver::new(&db)))
    .trigger_source(Box::new(ManualTriggerSource))
    .event_sink(Box::new(tui_event_sink))        // optional; host picks delivery
    .build()?;

engine.run(&workflow_def, inputs)?;
engine.resume(run_id)?;
```

---

## DSL Changes

The DSL grammar needs two small changes to become harness-agnostic:

**1. `foreach over` becomes an open string**

```
// Today (closed enum)
foreach { over = tickets | repos | workflow_runs }

// After (open string — validated against registered providers at runtime)
foreach { over = "tickets" }
foreach { over = "inbox" }        // communication harness
foreach { over = "pr-queue" }     // hypothetical CI harness
```

Existing `.wf` files are backwards-compatible — `tickets`, `repos`, `workflow_runs` stay as
the default conductor provider names.

**2. `trigger` validated at runtime, not parse time**

Currently the parser accepts `manual | pr | scheduled` and warns on unknowns. After extraction,
any string is valid syntax; the engine validates against registered `TriggerSource` names at
startup.

**3. `call` supports a harness-defined action registry**

Today `call plan` resolves to an agent `.md` file. In the extracted engine, `call` is
dispatched to the registered `ActionExecutor` whose `name()` matches the call identifier.
Conductor registers one executor per agent file; other harnesses register typed executors
(e.g., `send-email`, `post-slack`).

The agent `.md` resolution logic moves into `ClaudeAgentExecutor`, not the engine core.

---

## Communication Harness: Worked Example

To make the abstraction concrete, here is what the communication routing harness would look like.

### Harness setup

```rust
let engine = FlowEngine::builder()
    .persistence(Box::new(SqliteWorkflowPersistence::new(&db)))
    .run_context(Box::new(MessageRunContext::from_email(&email)))
    .action(Box::new(SendEmailExecutor::new(&smtp_config)))
    .action(Box::new(PostSlackExecutor::new(&slack_token)))
    .action(Box::new(CreateJiraTicketExecutor::new(&jira_config)))
    .action(Box::new(SummarizeWithLlmExecutor::new(&anthropic_key)))
    .item_provider(Box::new(InboxProvider::new(&imap_config)))
    .gate_resolver(Box::new(SlackReactionGateResolver::new(&slack_token)))
    .gate_resolver(Box::new(EmailReplyGateResolver::new(&imap_config)))
    .trigger_source(Box::new(ImapTriggerSource::new(&imap_config)))
    .build()?;
```

### Example workflow: email triage router

```
workflow triage-inbound-email {
  meta {
    description = "Route inbound email to the right channel or queue"
    trigger     = "imap"
    targets     = ["inbox"]
  }

  call summarize-with-llm { output = "triage-result" }

  if triage-result.is_support_request {
    call create-jira-ticket
    call send-email {
      inputs = { to = "{{sender_email}}", template = "support-ack" }
    }
  }

  if triage-result.is_sales_inquiry {
    call post-slack {
      inputs = { channel = "#sales-leads", message = "New inquiry from {{sender_email}}: {{subject}}" }
    }
    gate slack-reaction {
      prompt  = "React with ✅ to claim, ❌ to discard"
      timeout = "4h"
      on_timeout = continue
    }
  }

  if triage-result.needs_human_review {
    call post-slack {
      inputs = { channel = "#inbox-review", message = "Unclassified email from {{sender_email}}" }
    }
    gate slack-reaction { timeout = "24h"; on_timeout = fail }
  }

  always {
    call archive-message
  }
}
```

The DSL is identical to a conductor workflow. Only the harness registration changes.

---

## Persistence Boundaries

`runkon-flow` defines the *workflow* schema — `workflow_runs`, `workflow_run_steps`,
`workflow_run_step_fan_out_items` — via `SqliteWorkflowPersistence` (or any other
backend implementing `WorkflowPersistence`). It does **not** dictate where the
database lives. Each harness picks its own storage location by passing the
appropriate `Connection` / `PathBuf` / connection pool into the persistence impl at
`FlowEngineBuilder::persistence(...)` time.

### Distinct databases by default

Each harness owns its own database, combining:

- **Workflow tables** — defined and migrated by `runkon-flow` via
  `SqliteWorkflowPersistence`. Schema is identical across harnesses on the same
  `runkon-flow` version.
- **Domain tables** — entirely harness-specific. conductor-developer owns `repos`,
  `tickets`, `worktrees`, `agent_runs`, `repo_issue_sources`; comm-harness would
  own `inbox_messages`, `threads`, `slack_channels`, etc. No overlap.

Example layout:

```
~/.runkon/
├── developer.db         # workflow_* tables + conductor domain tables
└── inbox.db             # workflow_* tables + inbox domain tables
```

Why distinct by default:

- **Harness isolation.** Domain data from one harness has no business in another's
  table space. Operator mental model stays clean.
- **Independent schema evolution.** Harnesses ship on different cadences. Sharing
  a DB would force migration coordination across harnesses.
- **Operational simplicity.** Different backup, retention, and access patterns.
- **Multi-tenant safety.** A user running both harnesses for unrelated projects
  doesn't cross data.

### Shared-database deployments

Technically supported but not the default. Both engines can be configured to point
at the same `Connection` / file:

```rust
let shared = rusqlite::Connection::open("~/.runkon/shared.db")?;
let developer_engine = FlowEngine::builder()
    .persistence(Box::new(SqliteWorkflowPersistence::new(&shared)))
    // ...
    .build()?;
let inbox_engine = FlowEngine::builder()
    .persistence(Box::new(SqliteWorkflowPersistence::new(&shared)))
    // ...
    .build()?;
```

Workflow tables are shared (unified cross-harness run visibility). Domain tables
coexist without conflict — table names don't collide.

**Caveat:** Harnesses sharing a database must run compatible `runkon-flow`
versions. A schema migration introduced by one library version affects every
harness pointed at that DB. Shared deployments are only safe when both harnesses
are bundled and versioned together (e.g., a single multi-harness binary), or
when operators explicitly coordinate upgrades.

### Backend-agnostic by design

`WorkflowPersistence` is a trait — the SQLite default is the reference
implementation, not the contract. A harness with different durability needs can
implement a Postgres-backed `PostgresWorkflowPersistence`, a Redis-backed
`RedisWorkflowPersistence`, or an in-memory impl for tests (`runkon-flow` ships
`InMemoryWorkflowPersistence` for exactly this). Harnesses running against
different backends can never share state — that's a feature, not a bug.

---

## Events / Observability

Host applications (TUI, web UI, metrics systems, audit logs) need to learn about
workflow state changes in real time. Polling the database works but has
unacceptable latency for user-facing UIs. `runkon-flow` exposes an optional
event stream that fires on every state transition.

### `EventSink` trait

```rust
pub trait EventSink: Send + Sync {
    /// Emit a single event. Called synchronously from the engine thread;
    /// expected to be cheap. Sinks that need async dispatch (HTTP POST,
    /// database writes, etc.) must offload internally (e.g., via an mpsc
    /// channel).
    ///
    /// Sink panics are caught and logged — they must not tank the run.
    fn emit(&self, event: &EngineEventData);
}

pub struct EngineEventData {
    pub timestamp: DateTime<Utc>,
    pub event: EngineEvent,
}

#[non_exhaustive]
pub enum EngineEvent {
    // Run lifecycle
    RunStarted   { run_id: String, workflow_name: String, inputs: HashMap<String, String> },
    RunCompleted { run_id: String, status: RunStatus, error: Option<String> },
    RunResumed   { run_id: String, from_step_id: String },
    RunCancelled { run_id: String, reason: String },

    // Step lifecycle
    StepStarted   { run_id: String, step_id: String, step_kind: StepKind, position: Vec<usize> },
    StepCompleted { run_id: String, step_id: String, status: StepStatus, duration_ms: u64 },
    StepRetrying  { run_id: String, step_id: String, attempt: u32 },

    // Gate-specific
    GateWaiting  { run_id: String, step_id: String, gate_type: String, prompt: Option<String> },
    GateResolved { run_id: String, step_id: String, resolution: GateResolution },

    // Fan-out
    FanOutItemsCollected { run_id: String, step_id: String, item_count: usize },
    FanOutItemStarted    { run_id: String, step_id: String, item_id: String, item_label: String },
    FanOutItemCompleted  { run_id: String, step_id: String, item_id: String, status: ItemStatus },

    // Metrics (opt-in — emitted after cost/token accounting updates)
    MetricsUpdated { run_id: String, total_cost_usd: Option<f64>, total_tokens: i64, total_duration_ms: u64 },
}
```

Both `EngineEvent` and `EngineEventData` are `#[non_exhaustive]` — new variants
and fields can be added without a semver-major break.

### Registration

Multiple sinks can be registered; they receive every event in registration order.

```rust
let engine = FlowEngine::builder()
    .persistence(Box::new(SqliteWorkflowPersistence::new(&db)))
    .event_sink(Box::new(conductor_core::workflow::ChannelEventSink(tx))) // TUI live updates (lives in conductor-core, not runkon-flow)
    .event_sink(Box::new(PrometheusEventSink::new()))     // metrics
    .event_sink(Box::new(AuditLogEventSink::new(&path)))  // audit trail
    // ...
    .build()?;
```

### Semantics

- **Events are best-effort, not canonical.** The database is the source of
  truth. Events describe transitions as they happen; a crashed sink doesn't lose
  data (the DB already has it).
- **DB writes happen before event emission.** Subscribers never observe an event
  for state that isn't yet persisted.
- **Ordering within a run is preserved.** Emission is synchronous on the engine
  thread, so events for a single run arrive in transition order. Events from
  different runs may interleave.
- **Sinks that block, block the engine.** Default behavior: emission is
  synchronous. Slow sinks (HTTP calls, disk writes) must offload internally —
  typically by wrapping an `mpsc::Sender` and dispatching on a worker thread.
- **Sink panics are swallowed.** The engine catches, logs, and continues. A
  misbehaving sink must not tank a workflow.
- **No default sink.** `runkon-flow` has no opinion on how a host surfaces
  events. Each harness registers what it needs.
- **In-process only.** `EventSink` is not a cross-process mechanism. Hosts that
  need cross-process event delivery keep polling the DB — that's always safe
  because the DB update precedes the event emission.

### Non-goals

- **Event replay from history.** Events are live. If a sink misses them (not
  registered yet, crashed), they're gone. Reconstructing state requires reading
  from `WorkflowPersistence`.
- **Backpressure management.** Default is "slow sink blocks engine." Hosts that
  need drop-on-full or bounded-buffer semantics implement them in their own
  sinks.
- **Automatic retry.** If emission fails, the engine doesn't retry. Sinks that
  want at-least-once delivery implement retry internally.

---

## Cancellation

Five distinct triggers need to halt running work: user-initiated cancellation,
parallel `fail_fast`, step-level `timeout`, engine/host shutdown, and
parent-workflow cancellation propagating to sub-workflows. All five need to
reach whatever executor code is currently running.

### Model — cooperative + advisory preempt

- **Cooperative token** is the primary mechanism. Executors check
  `ectx.cancellation.is_cancelled()` at natural interruption points and exit
  early.
- **Advisory `ActionExecutor::cancel(execution_id)`** is the escalation for
  external work that can't observe the token (Claude subprocess already in
  flight, HTTP call mid-request). The engine fires `cancel()` and moves on —
  it does not wait for the executor to finish. Executors use this to kill
  subprocesses, abort connections, etc.

### `ExecutionContext` bundling struct

Runtime concerns the engine passes through to executors live on a single
struct rather than being scattered across params types:

```rust
pub struct ExecutionContext<'a> {
    pub run: &'a dyn RunContext,
    pub cancellation: &'a CancellationToken,
    // Future: tracing span, feature flags, request-scoped telemetry, etc.
}
```

Every executor trait method takes `&ExecutionContext` instead of `&dyn
RunContext` directly. Note: `RunContext` (the trait) and `ExecutionContext`
(the struct) are distinct types — the struct holds a reference to a trait
object. The earlier naming question (Open Q #7) resolved the *trait* name;
`ExecutionContext` claims the struct name because `ExecutionState` is being
removed in Step 1.1b.

### `CancellationToken`

```rust
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

impl CancellationToken {
    pub fn new() -> Self;

    /// Create a child token. Parent cancel propagates to child; child cancel
    /// does NOT propagate back to parent.
    pub fn child(&self) -> Self;

    pub fn cancel(&self, reason: CancellationReason);

    /// True if this token OR any ancestor has been cancelled.
    pub fn is_cancelled(&self) -> bool;

    pub fn reason(&self) -> Option<CancellationReason>;

    pub fn error_if_cancelled(&self) -> Result<(), EngineError>;
}

pub enum CancellationReason {
    UserRequested(Option<String>),   // from cancel_run() API
    Timeout,                         // step-level timeout fired
    FailFast,                        // sibling parallel branch failed
    ParentCancelled,                 // inherited from parent scope
    EngineShutdown,                  // host process shutting down
}
```

### Inheritance / scope tree

- **Run root token** — owned by the engine for each active run; cancelled by
  external `cancel_run()` or by engine shutdown.
- **Parallel scope token** — child of run root; one per parallel block.
  Cancelling the scope stops all branches (fail_fast).
- **Parallel branch token** — child of the parallel scope; one per branch.
- **Step token** — child of the enclosing scope; used for step-level
  timeouts (`timeout = "5m"` fires a timer that cancels this token).
- **Sub-workflow root token** — child of the parent step's token; parent
  cancel propagates downward into `call workflow` invocations.

### External cancel API

```rust
impl FlowEngine {
    pub fn cancel_run(
        &self,
        run_id: &str,
        reason: CancellationReason,
    ) -> Result<(), EngineError>;
}
```

Same-process flow:

1. Mark run as `Cancelling` via `WorkflowPersistence::update_run_status`.
2. Look up the in-memory root token for this run.
3. `token.cancel(reason)` — propagates to all descendants.
4. Spawn a thread to call `executor.cancel(execution_id)` for the running
   executor. Fire-and-forget; engine doesn't wait.
5. Return immediately. Engine's worker thread observes cancelled at next
   check, cleans up, marks run `Cancelled`, emits `RunCancelled` event.

Cross-process flow (CLI cancels a run owned by another process):

- Caller writes `Cancelling` status to DB.
- Owning process polls DB at step boundaries (already part of the resumability
  model), observes `Cancelling`, flips its in-memory token.
- Proceeds as same-process from step (3).

### Step boundaries are guaranteed interruption points

Even if an executor ignores the cooperative token, the engine checks
`is_cancelled()` before starting each step. A non-cooperating executor can
delay cancellation by the duration of its current step, but cannot prevent
it once the step completes.

### Resumability

**Cancelled runs are terminal — not resumable.** Resume is for crashes and
transient failures; cancellation is intentional (user, timeout, fail_fast).
Reversing a cancellation would require state rollback semantics, which are
out of scope.

### Shipping layers

- **Layer A (Phase 1):** Types — `ExecutionContext`, `CancellationToken`,
  `CancellationReason`. Trait signatures updated to take `&ExecutionContext`.
  Cooperative checks added in conductor executors where cheap. This shapes
  Steps 1.2, 1.3, 1.4.
- **Layer B (Phase 2):** `FlowEngine::cancel_run()`, in-memory token
  registry, parallel fail_fast wiring, step-level timeout → `token.cancel()`,
  `ActionExecutor::cancel()` escalation, cross-process DB-backed propagation,
  `RunCancelled` event emission, `ConductorClaudeAgentExecutor::cancel()`
  killing the subprocess (via PID).

---

## What Stays in Each Layer

### `runkon-flow` (the published library)

- DSL lexer, parser, AST (`WorkflowDef`, `WorkflowNode`, all node types)
- Engine execution loop (`execute_nodes`, `execute_single_node`)
- All node executors: call, if/unless, while/do_while, do, parallel, gate, always, script, foreach
- Workflow composition (`call workflow`) and depth/cycle detection
- Resumability and snapshot semantics
- Context threading (`prior_context`, `prior_contexts`, `{{variable}}` substitution)
- `WorkflowPersistence` trait + `InMemoryWorkflowPersistence` (for tests)
- All six traits defined above (`ActionExecutor`, `ItemProvider`, `GateResolver`,
  `TriggerSource`, `RunContext`, `WorkflowPersistence`)
- `EventSink` trait + `EngineEvent` / `EngineEventData` types (ships no default
  sink — hosts register their own)
- `ExecutionContext` struct, `CancellationToken`, `CancellationReason` enum
- `FlowEngine::cancel_run(run_id, reason)` external cancellation API
- `FlowEngine` builder

### `conductor-core` (conductor's harness layer)

- `ClaudeAgentExecutor` — agent `.md` resolution, headless subprocess spawn, `CONDUCTOR_OUTPUT` parsing
- `TicketsProvider`, `ReposProvider`, `WorkflowRunsProvider`, `WorktreesProvider`
- `PrApprovalGateResolver`, `PrChecksGateResolver`, `HumanApprovalGateResolver`
- `WorktreeRunContext` — resolves conductor-specific injected variables
- `SqliteWorkflowPersistence` — the existing schema
- All other conductor domain logic (repos, worktrees, tickets, agent runs)

### `comm-harness` (new, separate repo or crate)

- `ImapTriggerSource`, `WebhookTriggerSource`, `CronTriggerSource`
- `SendEmailExecutor`, `PostSlackExecutor`, `HttpRequestExecutor`, `SummarizeWithLlmExecutor`
- `InboxProvider`, `SlackChannelProvider`
- `SlackReactionGateResolver`, `EmailReplyGateResolver`
- `MessageRunContext`

---

## Packaging Strategy

### Option A: Monorepo, new `runkon-flow` crate

Add `runkon-flow/` to the conductor-ai workspace. Conductor depends on it. The comm-harness
is a separate repo that also depends on it. `runkon-flow` is published to crates.io.

**Pro:** Single place to develop and iterate the engine. Conductor CI catches regressions.  
**Con:** Conductor repo owns a general-purpose library — governance gets blurry.

### Option B: Separate `runkon-flow` repo

Extract `runkon-flow` into its own repo. Both conductor and comm-harness depend on it as a
crates.io dependency.

**Pro:** Clean separation of concerns. Library has its own versioning, changelog, issues.  
**Con:** Cross-repo development friction when conductor needs an engine change.

### Recommendation

Start with **Option A** during the extraction phase — it's lower friction and lets the
trait interfaces stabilize against two real harnesses before publishing. Extract to a
separate repo when the API is stable (end of Phase 2 in the migration plan below).

---

## Migration Plan

### Ordering Rationale

The six traits are not independent. `ItemProvider`, `GateResolver`, `ActionExecutor`,
and `TriggerSource` all take `&dyn RunContext` in their method signatures, so
`RunContext` is the keystone — if any other trait lands first, it has to be built
against concrete `ExecutionState` fields and then refactored a second time once
`RunContext` exists. `WorkflowPersistence` is the widest surface (~13 methods over
runs, steps, fan-out items) and its record types are shaped by what the executors
store, so persistence goes last to avoid churning the trait twice. Within that
frame, the order below prefers narrow, self-contained changes (gates, foreach)
before wide ones (call/action, persistence), so early steps exercise the
trait-and-registry pattern on small surfaces and build confidence.

Two alternatives worth flagging before committing:

- **Lead with `GateResolver` instead of `RunContext`** to de-risk the pattern on a
  smaller target first. Cost: some `&dyn RunContext` parameters become concrete
  state references temporarily and get refactored when `RunContext` lands. Cheap
  to do because gate.rs is self-contained.
- **Answer the async question (Open Q #2) before step 4.** If `runkon-flow` stays
  sync and executors spawn their own threads, `ActionExecutor` is a much smaller
  trait than if the engine goes async. Worth deciding explicitly before extracting
  the `call` path.

### Phase 1 — Internal trait refactor (~2 weeks)

All six trait extractions happen in `conductor-core` with no public API or DSL
breakage. Existing behavior is preserved end-to-end; every step should keep the
~800 workflow tests and ~240 DSL tests green.

**Step 1.1 — `RunContext` (keystone, split into two sub-steps)**

- 1.1a: Introduce `RunContext` as a *facade* over the existing 13–15 domain
  fields in `ExecutionState` (`ticket_id`, `repo_id`, `worktree_id`,
  `worktree_slug`, `repo_path`, `working_dir`, plus the 9 keys in
  `ENGINE_INJECTED_KEYS`). Do not delete the concrete fields yet. Migrate
  `prompt_builder.rs` variable resolution and the `apply_workflow_input_defaults`
  path to read through the trait.
- 1.1b: Migrate remaining executors and `manager/` callers to the trait, then
  delete the concrete fields from `ExecutionState`. After this,
  `ENGINE_INJECTED_KEYS` becomes `WorktreeRunContext::injected_variables()`.

**Step 1.2 — `GateResolver`**

- Extract the inline `gh pr view --json reviews,author` and `gh pr checks`
  subprocess calls from `executors/gate.rs` into `PrApprovalGateResolver` and
  `PrChecksGateResolver`.
- Leave the existing human-approval DB fast-path in `HumanApprovalGateResolver`.
- Executor dispatches to a `HashMap<String, Box<dyn GateResolver>>` keyed by
  `gate_type()`. Self-contained to one file; smallest high-value extraction.

**Step 1.3 — `ItemProvider`**

- Replace the closed `ForeachOver` enum (Tickets | Repos | WorkflowRuns |
  Worktrees) with an open string in the DSL AST.
- Parser continues to accept the four historic names; engine registers
  `TicketsProvider`, `ReposProvider`, `WorkflowRunsProvider`, `WorktreesProvider`
  with those names so existing `.wf` files and the ~40 foreach tests stay green.
- The four match arms in `executors/foreach/mod.rs` collapse into a single
  registry lookup.

**Step 1.4 — `ActionExecutor`**

- Lower risk than it appears: the headless Claude spawn already lives in
  `agent_runtime/`, so `ClaudeAgentExecutor` is a thin wrapper around the
  existing `try_spawn_headless_run` entry point plus the direct-API path.
- Keep `ActionOutput` shape aligned with what `output.rs` already parses from
  `CONDUCTOR_OUTPUT` blocks (markers + context + metadata) — don't redesign
  during extraction.
- Prerequisite: decide sync vs. async (Open Q #2) before starting this step.

**Step 1.5 — `WorkflowPersistence` (last in Phase 1)**

- Trait-ify the ~13 methods currently exposed by `WorkflowManager` across
  `manager/lifecycle.rs`, `manager/steps.rs`, `manager/queries.rs`, and
  `manager/fan_out.rs`.
- `SqliteWorkflowPersistence` wraps the existing schema (`workflow_runs`,
  `workflow_run_steps`, `workflow_run_step_fan_out_items`) unchanged.
- Ship an `InMemoryWorkflowPersistence` in the same PR for test usage.

**Step 1.6 (optional, can defer to Phase 5) — `TriggerSource`**

- Implement as first-class instead of the current stubs.
- Defer until the comm-harness needs it; extracting it in isolation gives no
  conductor-visible win.

### Phase 2 — Extract `runkon-flow` crate (~1 week)

- Add `runkon-flow/` to the workspace (monorepo, Option A in "Packaging Strategy").
- Move DSL lexer, parser, AST, engine execution loop, all node executors, the
  six trait definitions, `InMemoryWorkflowPersistence`, and `FlowEngineBuilder`
  into `runkon-flow`.
- `conductor-core` becomes a `runkon-flow` consumer. Domain-specific
  implementations (`ClaudeAgentExecutor`, the three `ItemProvider`s, the three
  `GateResolver`s, `WorktreeRunContext`, `SqliteWorkflowPersistence`) stay in
  `conductor-core`.
- **Pre-publication API cleanup (per Open Q #3):** Extract `RunContext::script_env()`
  into a dedicated `ScriptEnvProvider` trait. Move `conductor_bin_dir` /
  `extra_plugin_dirs` from `WorktreeRunContext` into a new
  `ConductorScriptEnvProvider`. Register via `FlowEngineBuilder::script_env_provider(...)`.
  Must land before the crates.io publish — post-publication it becomes a
  semver-major break. Estimated ~1–2 hours.
- **Harness discovery validation (per Open Q #4):** Add
  `FlowEngine::validate(&self, def: &WorkflowDef) -> Result<(), Vec<ValidationError>>`.
  Walks the AST and checks every `call`, `foreach over`, and `gate` reference against the
  engine's registered `ActionExecutor` / `ItemProvider` / `GateResolver` names. Recurses
  into `call workflow` sub-workflows via the `WorkflowResolver`. Called automatically by
  `FlowEngine::run()` before execution; also public for CI lint tools. Rolls up the
  partial dispatch-time errors added during Phase 1 Steps 1.2–1.4 into a single
  unified validation pass.
- **`WorkflowResolver` trait (per Open Q #5):** Define the trait in `runkon-flow`,
  ship `DirectoryWorkflowResolver` (filesystem, re-read on each resolve for hot-reload)
  and `InMemoryWorkflowResolver` (tests). Builder: `.workflow_dir(&Path)` convenience +
  `.workflow_resolver(Box<dyn WorkflowResolver>)` override. Conductor wires
  `.workflow_dir(".conductor/workflows")`; existing sub-workflow resolution logic in
  `conductor-core` moves into `DirectoryWorkflowResolver`.
- Publish `runkon-flow 0.1.0-alpha` to crates.io. Do not stabilize the API yet —
  wait for Phase 5 validation.

### Phase 3 — Wire conductor-core through runkon-flow's FlowEngine (~1–2 weeks)

Phase 2 built `runkon-flow` as a parallel, complete workflow engine — DSL, traits,
execution loop, all node executors, `FlowEngineBuilder`, `WorkflowResolver`, `validate()`,
`EventSink`, cancellation — but did **not** switch conductor-core to use it. As of
Phase 2, `conductor-core` depends on `runkon-flow` only for a handful of re-exports
(`EngineError`, `CancellationReason`, `ScriptEnvProvider`). The CLI and TUI call
`execute_workflow_standalone` / `resume_workflow_standalone`, which go entirely through
conductor-core's own `engine.rs` using `crate::workflow_dsl`. The two engines are
parallel, not wired together.

Until this phase completes, the comm-harness (Phase 5) would be using a different engine
than conductor, and the Phase 4 persistence work would be migrating out of an engine
that conductor doesn't actually run through.

**Step 3.1 — Replace `execute_workflow_standalone` internals with `FlowEngine::run()`**

- Wire `execute_workflow_standalone` to build a `FlowEngine` via `FlowEngineBuilder`,
  register conductor's trait implementations (`ClaudeAgentExecutor` as fallback,
  the four `ItemProvider`s, the three `GateResolver`s, `WorktreeRunContext`,
  `SqliteWorkflowPersistence`), and call `runkon_flow::FlowEngine::run(&workflow_def, inputs)`.
- The function signature and return type are unchanged — all callers (CLI, TUI, web) are unaffected.
- Run the full test suite after each sub-step to catch divergence early.

**Step 3.2 — Replace `resume_workflow_standalone` internals with `FlowEngine::resume()`**

- Wire `resume_workflow_standalone` to call `runkon_flow::FlowEngine::resume(run_id)`.
- Gate resumability (precondition checks, `validate_resume_preconditions`) stays in
  conductor-core — these are conductor domain rules, not engine rules.

**Step 3.3 — Delete conductor-core's own engine and DSL**

Once Steps 3.1 and 3.2 are green end-to-end:
- Delete `conductor-core/src/workflow/engine.rs` (the old execution loop).
- Delete `conductor-core/src/workflow_dsl/` (the old DSL copy: lexer, parser, types,
  validation, api, script_utils, tests).
- Update all `crate::workflow_dsl::` import sites to use `runkon_flow::dsl::` instead.
- Update `conductor-core/src/workflow/mod.rs` re-exports to source from `runkon_flow`.
- After deletion, `cargo test --workspace` must stay green. This is the validation gate
  that confirms the migration is complete.

**Step 3.4 — Delete conductor-core's duplicate `FlowEngineBuilder`**

`conductor-core/src/workflow/flow_engine.rs` contains its own `FlowEngineBuilder` that
only builds an `ActionRegistry` — a partial reimplementation of the full builder in
`runkon-flow`. Once conductor-core delegates to `runkon_flow::FlowEngine`, this file
is dead code and should be deleted. Callers are updated to use `runkon_flow::FlowEngineBuilder`.

### Phase 4 — Consolidate persistence implementations (~1 week)

Phase 2 left `SqliteWorkflowPersistence` in `conductor-core` and
`InMemoryWorkflowPersistence` duplicated across both crates. Phase 4 fixes
both so that any future harness gets a production-ready SQLite backend for free
from `runkon-flow`, without writing its own.

**Current state (post Phase 2):**
- `InMemoryWorkflowPersistence` exists in **both** `conductor-core/src/workflow/persistence_memory.rs`
  (674 lines, uses `crate::workflow::` paths) and `runkon-flow/src/persistence_memory.rs`
  (747 lines, canonical). Structurally identical implementations with diverged imports.
- `SqliteWorkflowPersistence` lives in `conductor-core/src/workflow/persistence_sqlite.rs`
  (368 lines). Blocked from moving to `runkon-flow` by three dependencies:
  `WorkflowManager` (conductor's SQL manager — used as a transient delegate for every
  method), `ConductorError` (conductor-specific error type), and conductor-internal
  types (`FanOutItemRow`, `WorkflowRun`, `WorkflowRunStep`) imported via `crate::workflow::`.
- `runkon-flow` has **no `rusqlite` dependency** — it is pure Rust with no DB deps.

**Step 4.1 — Consolidate `InMemoryWorkflowPersistence`** (quick win, ~2 hours)

- Delete `conductor-core/src/workflow/persistence_memory.rs`.
- Update all `conductor-core` import sites to use
  `runkon_flow::persistence_memory::InMemoryWorkflowPersistence` directly.
- No behavior change — the two implementations are structurally identical; the
  conductor-core copy has simply fallen behind the canonical `runkon-flow` version.

**Step 4.2 — Break `SqliteWorkflowPersistence`'s `WorkflowManager` dependency** (~3 hours)

`SqliteWorkflowPersistence` currently works like this:

```rust
fn create_run(&self, ...) -> Result<String, EngineError> {
    let conn = self.conn.lock()?;
    let mgr = WorkflowManager::new(&conn);   // transient delegate
    mgr.create_run(...).map_err(|e| EngineError::Persistence(e.to_string()))
}
```

- Rewrite each method to use `rusqlite` directly, inlining the SQL that `WorkflowManager`
  currently provides. The SQL is straightforward CRUD against
  `workflow_runs`, `workflow_run_steps`, and `workflow_run_step_fan_out_items`.
- After this step, `persistence_sqlite.rs` imports only: `rusqlite`, `EngineError`,
  and the `WorkflowPersistence` trait types. No `ConductorError`, no `WorkflowManager`,
  no `crate::workflow::` paths.
- `WorkflowManager` is unaffected — it continues to exist in `conductor-core` for other
  callers (lifecycle, queries, fan-out). This step only removes the delegation pattern
  inside `SqliteWorkflowPersistence`.

**Step 4.3 — Move `SqliteWorkflowPersistence` to `runkon-flow` as an optional feature** (~2 hours)

- Add to `runkon-flow/Cargo.toml`:
  ```toml
  [features]
  sqlite = ["dep:rusqlite"]

  [dependencies]
  rusqlite = { version = "0.31", features = ["bundled"], optional = true }
  ```
- Move `persistence_sqlite.rs` to `runkon-flow/src/persistence_sqlite.rs`, gate the
  module with `#[cfg(feature = "sqlite")]`.
- Update `conductor-core/Cargo.toml` to depend on `runkon-flow` with
  `features = ["sqlite"]`.
- `conductor-core` re-exports `SqliteWorkflowPersistence` from its own `workflow` module
  for backwards compatibility with existing callers.
- The comm-harness (Phase 5) gets a production-ready SQLite backend by simply enabling
  the `sqlite` feature — no implementation work required.

**Step 4.4 — Schema migration ownership (deferred to post-Phase 5)**

The ~10 workflow migration files in `conductor-core/src/db/migrations/` (
`020_workflow_runs.sql`, `021_workflow_redesign.sql`, etc.) define the schema that
`SqliteWorkflowPersistence` expects. `runkon-flow` can document the required schema
without owning the migration files — conductor continues to manage migrations for its
own DB. Full schema ownership transfer is deferred until the comm-harness (Phase 5)
reveals what its persistence needs look like and whether the schema needs to evolve for
multi-harness use.

**DB topology:** `runkon-flow` has no opinion about which database the workflow tables
live in. `SqliteWorkflowPersistence::new(conn)` accepts whatever connection the harness
passes — the harness decides the topology. Two patterns:

- **Conductor (shared DB):** workflow tables live alongside `worktrees`, `repos`,
  `agent_runs`, etc. in `~/.conductor/conductor.db`. This is required, not optional:
  `workflow_runs` carries FK references to `worktrees.id` and `repos.id`, and
  `agent_runs` references `workflow_runs` in the other direction. SQLite has no
  cross-database FK enforcement, so moving workflow tables to a separate file would
  silently drop referential integrity and break cross-table JOINs. Single DB also
  preserves transaction atomicity across workflow + harness state changes.
- **comm-harness (fresh start):** no FK entanglement with external tables, so it can
  put everything in one DB or hand `SqliteWorkflowPersistence` a dedicated
  `workflow.db` connection — either works.

**Schema self-description:** when `SqliteWorkflowPersistence` moves to `runkon-flow`
in Step 4.3, it should expose `SqliteWorkflowPersistence::create_tables(&conn)` — the
authoritative DDL for the three workflow tables. Conductor's migration runner invokes
this (or mirrors the DDL in an explicit migration file); the comm-harness calls it
directly as its sole initial migration. Future workflow schema changes are coordinated
as a `runkon-flow` semver bump plus a new migration file in each harness.

### Phase 5 — Second harness + stabilize (~2–3 weeks)

- Build `comm-harness` in a separate repo depending on `runkon-flow` from crates.io.
- Implement `ImapTriggerSource`, `SendEmailExecutor`, `PostSlackExecutor`,
  `InboxProvider`, `SlackReactionGateResolver`, `MessageRunContext`.
- This is where trait gaps surface (harness discovery validation, `call
  workflow` resolution, `PATH` injection — see Open Questions).
- Publish `runkon-flow 0.1.0` stable once the comm-harness ships end-to-end.
- If cross-repo development friction becomes painful during Phase 5, extract
  `runkon-flow` from the conductor workspace into its own repo (Option B) before
  stabilizing.

---

## Open Questions

1. **`call` name resolution:** *Resolved (2026-04-19) — named registry + catch-all
   fallback.* The builder exposes `.action(...)` (registers by `name()`) and
   `.action_fallback(...)` (catch-all). Named executors take precedence; fallback
   handles misses. Conductor registers one `ClaudeAgentExecutor` as fallback that
   dispatches internally by `params.name` to `.md` files — preserves hot-reload behavior
   (new agent files work without engine re-init). Comm-harness registers typed
   executors per action name. Both patterns compose — a harness can register a
   `SpecialAgentExecutor` by name AND `ClaudeAgentExecutor` as fallback. At most one
   fallback per engine; second registration errors at `build()`. No trait signature
   changes — `ActionExecutor::name()` stays as-is (diagnostic for fallbacks), and
   `ActionParams.name` (already in §1) carries the DSL-level call name so fallback
   executors can dispatch internally. Rejected: strict named registry (forces conductor
   to enumerate N executors or dynamic-register, breaks hot reload); single
   `ActionDispatcher` trait (loses per-action encapsulation); pluralized `names()`
   method (requires startup filesystem scan, breaks hot reload).

2. **Async:** *Resolved (2026-04-19) — sync-only.* `FlowEngine::run()` stays sync,
   all six traits stay sync (no `async fn`), parallel branches and foreach fan-out use
   `std::thread::spawn`, gate polling uses sync sleep loops. Executors that use async
   libraries internally build their own runtime or accept a `tokio::runtime::Handle` at
   construction and call `handle.block_on(future)`. Async host applications (axum/tokio)
   call into `FlowEngine::run()` via `tokio::task::spawn_blocking` — standard pattern.
   An `AsyncAction` helper wrapper is deferred to Phase 5 — ship when the comm-harness
   reveals what ergonomics actually matter. Rejected: async-first engine (2–3× the
   Phase 1 refactor scope, drags tokio into conductor-core/CLI/TUI, breaks the TUI
   threading model, no identified use case requires it — comm-harness works fine with
   sync crates); dual-mode sync+async traits (doubles maintenance, clunky tooling).
   Scale concerns (SaaS deployments with 10k+ concurrent runs) are addressed by
   *continuation-based execution*, not async — see
   [runkon-flow-scaling.md](./runkon-flow-scaling.md) for the three escape hatches.

3. **`script` step and environment injection:** *Resolved (2026-04-19) — Option 1 for
   Phase 1, Option 2 before publication.* For Phase 1 the engine exposes
   `RunContext::script_env() -> HashMap<String, String>` with a default-empty impl;
   harnesses compose `PATH` themselves (reading `std::env::var("PATH")` and prepending
   as needed) and return the full env map. `conductor_bin_dir` and `extra_plugin_dirs`
   move into `WorktreeRunContext` as private fields and leave `ExecutionState` entirely.
   Before publishing `runkon-flow 0.1.0-alpha` to crates.io, extract `script_env()` into
   a dedicated `ScriptEnvProvider` trait so that `RunContext` stays tight to its
   original scope (template vars + working_dir) and so shell concerns have room to
   grow (timeouts, hooks, output shaping) behind a clean seam. Rationale: Option 1
   is ~1–2h of work inside conductor-core with no DSL/schema/persistence churn; the
   Option 2 refactor is mechanical pre-publication (~1–2h) but becomes a semver-major
   break post-publication. Doing Option 1 first unblocks Step 1.1 immediately; doing
   Option 2 before publication gets the cleaner API out the door. Added as an explicit
   pre-publication task in Phase 2.

4. **Harness discovery:** *Resolved (2026-04-19) — public `FlowEngine::validate()`,
   rolled up in Phase 2.* Expose
   `FlowEngine::validate(&self, def: &WorkflowDef) -> Result<(), Vec<ValidationError>>`
   that walks the AST and checks every `call <name>` against registered
   `ActionExecutor::name()`, every `foreach over <name>` against registered
   `ItemProvider::name()`, and every `gate <type>` against registered
   `GateResolver::gate_type()`. Collects all errors (not fail-on-first). `FlowEngine::run()`
   calls it internally once before execution. Public so CI tools and linters can call it
   without running the workflow. During Phase 1 each executor's dispatcher returns a
   clean `"no registered X for 'name'"` error as a safety net. The unified validation
   lands as a single Phase 2 deliverable — shipping partial validation in Steps 1.2/1.3/1.4
   would give inconsistent per-kind coverage, which is a worse UX than deferring until all
   three trait registrations exist. Rejected: validation at `FlowEngineBuilder::build()`
   (couples engine init to filesystem scanning, too conductor-shaped); validation at
   parse time (couples pure parser to runtime engine state); dispatch-time-only
   (worst feedback — errors can hide inside conditionals for hours).

5. **`call workflow` resolution:** *Resolved (2026-04-19) — `WorkflowResolver` trait
   with built-in `DirectoryWorkflowResolver` and `InMemoryWorkflowResolver`.* Trait
   shape:

   ```rust
   pub trait WorkflowResolver: Send + Sync {
       fn resolve(&self, name: &str) -> Result<Arc<WorkflowDef>, EngineError>;
   }
   ```

   Builder exposes two registration paths: `.workflow_dir(&Path)` as a convenience
   that registers a `DirectoryWorkflowResolver` for the common filesystem case,
   and `.workflow_resolver(Box<dyn WorkflowResolver>)` for custom backends (DB, S3,
   dynamic generation). Conductor uses `.workflow_dir(".conductor/workflows")` —
   preserves hot-reload because `DirectoryWorkflowResolver` re-reads on each
   `resolve()` call. Harnesses that need caching implement their own resolver.
   Return type is `Arc<WorkflowDef>` so cached resolvers can share instances
   without cloning. Not-found returns `EngineError::WorkflowNotFound(name)`.
   `InMemoryWorkflowResolver` ships with `runkon-flow` for test use, matching the
   `InMemoryWorkflowPersistence` pattern. `FlowEngine::validate()` (#4) recurses
   through `call workflow` references via the resolver; existing depth-limit-of-5
   cycle detection stays in the engine. Rejected: eager registry (breaks
   conductor's hot-reload, awkward for dynamic workflow generation); resolver on
   `RunContext` (wrong abstraction layer — workflow resolution is engine-wide, not
   per-run). Resolver composition (layered resolvers, e.g., DB → filesystem
   fallback) deferred to user-space — not in scope for v1.

   *Lands in Phase 2 with the crate extraction.* During Phase 1 sub-workflow
   resolution stays as-is in `conductor-core` (filesystem walk). The trait only
   matters once `runkon-flow` is a separate crate.

6. **Naming:** *Resolved (2026-04-19) — `runkon-flow`.* Namespaced under the
   `runkon.ai` brand; `-flow` signals the purpose (workflow/flow engine) without
   boxing the crate in as "runkon's internal plumbing" the way `-core` would.
   Leaves room for sibling crates (`runkon-cli`, `runkon-web`) later.
   Rejected: `flowcore` (product collision with flowcore.com), `runkon-core`
   (reads as runkon-internal, discourages external adoption), `conductor-wf`
   (ties a general-purpose library to the conductor brand).

7. **Naming: trait name for injected context:** *Resolved (2026-04-19) — `RunContext`.*
   Avoids the `ExecutionState` / `ExecutionContext` collision (the infrastructure struct
   that stays after the refactor), matches existing domain vocabulary (`WorkflowRun`,
   `run_id`, `RunStatus`), and reads naturally with `FanOutItem.context` which merges
   into the child's `RunContext`.

8. **`working_dir` type:** *Resolved (2026-04-19) — `PathBuf`.* Idiomatic for filesystem
   paths, self-documenting at the trait boundary, consistent with the owned-return
   pattern used by `injected_variables()` and `script_env()`, and matches what
   `std::process::Command::current_dir` consumes natively. Per-call allocation cost is
   negligible at this call frequency. The existing `ExecutionState.working_dir: String`
   converts via `PathBuf::from(&s)` in the Step 1.1a facade; once Step 1.1b removes the
   concrete field, `WorktreeRunContext` stores `PathBuf` directly. Template substitution
   for `{{working_dir}}` renders via `.to_string_lossy()` (noted in the trait docstring).

9. **`HumanApprovalGateResolver` DB access:** *Resolved (2026-04-19) —
   constructor-injected, evolving across steps.* Step 1.2 ships
   `HumanApprovalGateResolver::new(db_path: PathBuf)` — the resolver opens its own
   `rusqlite::Connection` eagerly at construction and queries `workflow_run_steps`
   directly. Step 1.5 (when `WorkflowPersistence` lands) refactors this to
   `HumanApprovalGateResolver::new(persistence: Arc<dyn WorkflowPersistence>)`,
   moves the approval-state query into `WorkflowPersistence::get_gate_approval(step_id)`,
   and drops the direct connection. Rationale: keeps Step 1.2 unblocked without forward
   dependency on Step 1.5; avoids leaking `rusqlite::Connection` through `RunContext`
   (rejected — breaks the harness-agnostic promise); avoids inventing a separate
   `ApprovalStore` trait (rejected — duplicates what `WorkflowPersistence` already owns).
   Eager connection open preferred over lazy so init-time failures surface at the
   `FlowEngineBuilder::build()` call.

10. **GitHub token caching location:** *Resolved (2026-04-19) — shared
    `Arc<GitHubTokenCache>` concrete helper.* Introduce a concrete
    `GitHubTokenCache` struct in `conductor-core` that wraps the current
    `Mutex<Option<(String, Instant)>>` + `gh auth token` shell-out logic from
    `executors/gate.rs` (lines ~173–207). Both `PrApprovalGateResolver` and
    `PrChecksGateResolver` receive `Arc<GitHubTokenCache>` at construction and share
    it, so only one `gh auth token` call is made per TTL window across both resolvers.
    `GitHubTokenCache::new()` accepts an optional `token_override: Option<String>` for
    tests. Rejected: full `GitHubTokenProvider` trait (premature — one real
    implementation, easy to upgrade later); module-level static cache (global mutable
    state, un-resettable in tests); duplicated per-resolver caches (two shell-outs for
    no reason). Crosses no crate boundary — entirely internal to `conductor-core`.

11. **`WorktreesProvider` registration:** *Resolved (2026-04-19) — register alongside
    the other three.* §2, §"Engine Initialization", and §"What Stays in Each Layer"
    updated to list `WorktreesProvider` as the fourth conductor-core `ItemProvider`
    implementation. Maps cleanly to `FanOutItem` (id = worktree slug, label = branch
    name, context = `{worktree_id, repo_path, worktree_slug, ...}`). Deprecating
    `foreach over worktrees` was rejected — it's an existing feature and the
    breaking-change cost is wildly out of proportion to the marginal surface-area
    reduction. `WorktreesProvider::dependencies()` returns empty for now; cross-worktree
    ordering lives in the feature-branch coordination layer, not the foreach fan-out.

12. **`filter` parameter on `ItemProvider::items()`:** *Resolved (2026-04-19) — keep
    `filter: &HashMap<String, String>` in the trait signature.* Correcting an earlier
    misread of the codebase: `filter` is a live, load-bearing concept today, not
    speculative. `ForEachNode.filter: HashMap<String, String>` exists in the AST
    (`workflow_dsl/types.rs:232`), the DSL parses `filter = { key = "value" }`
    (`parser.rs:1050`), and the validator *requires* `filter` for
    `foreach over workflow_runs` with `filter.status` = terminal status
    (`validation.rs:354`). Concrete consumer: `WorkflowRunsProvider` reads
    `filter.get("status")` and `filter.get("workflow_name")` to build its SQL query
    (`executors/foreach/mod.rs:432-434`). Real user: `workflow-postmortem.wf` iterates
    over failed runs via `filter = { status = "failed" }`. The trait signature must
    match DSL reality. Per-provider filter semantics (required for workflow_runs,
    honored by tickets, warned-about for repos/worktrees) stay in the validator, not
    the trait — providers receive the raw map and decide what to do with it.
    `WorktreesProvider` and `ReposProvider` treat a non-empty filter as a no-op.
