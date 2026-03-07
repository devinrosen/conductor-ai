# Workflow Engine Redesign

## Problems with the current approach

1. **Topology and prompts are conflated** — the workflow file mixes execution graph with agent instructions. Prompts can't be reused across workflows.
2. **No loops** — the flat `steps` list can't express iteration. The engine can't track or enforce them.
3. **Condition evaluation is a keyword hack** — `contains()` on free text. An agent saying "there are no has_review_issues" evaluates to true. Control flow correctness depends on prompt discipline.
4. **No parallelism** — steps are strictly sequential. A review swarm can't be expressed.
5. **No error handling** — the only policy is a global `fail_fast` flag.
6. **No data passing between steps** — steps communicate only through filesystem and git state.

---

## File format

### Workflow file: `.conductor/workflows/<name>.wf`

A minimal custom DSL that describes only the execution graph. Uses a hand-written recursive descent parser (~400 lines of Rust). TOML was considered but rejected because it cannot naturally express polymorphic step arrays (`[[steps]]` mixed with `[[steps.loop]]` is not valid TOML structure).

```
workflow ticket-to-pr {
  meta {
    description = "Full development cycle"
    trigger     = "manual"
  }

  inputs {
    ticket_id  required
    skip_tests default = "false"
  }

  call plan

  call implement {
    retries = 2
    on_fail = diagnose
  }

  call push_and_pr
  call review

  while review.has_review_issues {
    max_iterations = 10
    stuck_after    = 3
    on_max_iter    = fail

    call address_reviews
    call push
    call review
  }

  parallel {
    fail_fast   = false
    min_success = 1
    call reviewer_security
    call reviewer_tests
    call reviewer_style
  }

  gate human_review {
    prompt     = "Review agent findings before merging. Add notes if needed."
    timeout    = "48h"
    on_timeout = fail
  }

  gate pr_checks {
    timeout    = "2h"
    on_timeout = fail
  }

  if review.has_critical_issues {
    call escalate
  }

  always {
    call notify_result
  }
}
```

#### Grammar (informal)

```
workflow_file  := "workflow" IDENT "{" meta? inputs? node* "}"
meta           := "meta" "{" kv* "}"
inputs         := "inputs" "{" input_decl* "}"
input_decl     := IDENT ("required" | "default" "=" STRING)
node           := call | if | while | parallel | gate | always
call           := "call" IDENT ("{" kv* "}")?
if             := "if" condition "{" kv* node* "}"
while          := "while" condition "{" kv* node* "}"
parallel       := "parallel" "{" kv* call* "}"
gate           := "gate" gate_type "{" kv* "}"
always         := "always" "{" node* "}"
condition      := IDENT "." IDENT
gate_type      := "human_approval" | "human_review" | "pr_approval" | "pr_checks"
kv             := IDENT "=" (STRING | NUMBER | IDENT)
```

#### Constructs

| Construct | Description |
|---|---|
| `call <agent>` | Run a named agent |
| `if <step>.<marker>` | Run body only if the named step's last output contains the marker |
| `while <step>.<marker>` | Repeat body until the named step's output no longer contains the marker |
| `parallel` | Run multiple `call` statements concurrently; merge their markers |
| `gate <type>` | Pause until an external condition is met |
| `always` | Run body regardless of workflow success or failure |

#### `call` options

| Option | Description |
|---|---|
| `retries = N` | Retry the step up to N times on failure |
| `on_fail = <agent>` | Agent to call if all retries are exhausted |

#### `while` options

| Option | Required | Description |
|---|---|---|
| `max_iterations` | Yes | Hard cap on loop iterations |
| `stuck_after` | No | Fail if the complete marker set is identical for N consecutive iterations |
| `on_max_iter` | No | `fail` (default) or `continue` when cap is reached |

#### `parallel` options

| Option | Description |
|---|---|
| `fail_fast = true\|false` | If true (default), cancel remaining agents when one fails |
| `min_success = N` | Minimum number of agents that must succeed; default is all |

#### `gate` options

| Option | Applies to | Description |
|---|---|---|
| `prompt` | human gates | Message shown to the approver |
| `min_approvals` | `pr_approval` | GitHub approvals required (default 1) |
| `timeout` | all | Duration: `"2h"`, `"24h"`, `"72h"` |
| `on_timeout` | all | `fail` (default) or `continue` |

---

### Agent file: `.conductor/agents/<name>.md`

Standalone. Any workflow can reference these.

```markdown
---
role: actor
can_commit: true
model: claude-opus-4-6
---

You are a software engineer. The ticket is: {{ticket_id}}

Prior step context: {{prior_context}}

Implement the plan written in PLAN.md.
```

**Frontmatter fields:**

| Field | Values | Description |
|---|---|---|
| `role` | `actor` \| `reviewer` | Semantic label for display and tooling. Does not hard-enforce behavior — `can_commit` is the enforcement mechanism. `reviewer` communicates read-only intent; `actor` communicates that side effects are expected. |
| `can_commit` | bool | Whether this agent is permitted to commit code to the branch |
| `model` | string | Optional model override for this agent |

**Agent resolution order** (first match wins):
1. `.conductor/workflows/<workflow-name>/agents/<name>.md` — workflow-local override
2. `.conductor/agents/<name>.md` — shared

---

## Structured output: `CONDUCTOR_OUTPUT`

Control flow conditions must not rely on scanning free text. Every agent prompt template automatically appends:

```
When you have finished your work, output the following block exactly as the
last thing in your response. Do not include this block in code examples or
anywhere else — only as the final output.

<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": ""}
<<<END_CONDUCTOR_OUTPUT>>>

markers: array of string signals consumed by the workflow engine
         (e.g. ["has_review_issues", "has_critical_issues"])
context: one or two sentence summary of what you did or found,
         passed to the next step as {{prior_context}}
```

The engine finds the last occurrence of `<<<CONDUCTOR_OUTPUT>>>` / `<<<END_CONDUCTOR_OUTPUT>>>` in the agent's output and parses the JSON between them. Using `<<<` delimiters makes accidental matches in code blocks unlikely.

Agents that omit the block are treated as emitting no markers and no context. This is not an error.

---

## Context threading

`{{prior_context}}` contains the `context` string from the immediately preceding step.

For cases where a step needs broader history (e.g., an `address_reviews` agent that needs to know it has already attempted this three times), the full accumulated context is available as `{{prior_contexts}}` — a JSON array of objects:

```json
[
  {"step": "plan",             "iteration": 0, "context": "Created PLAN.md with 4 tasks"},
  {"step": "implement",        "iteration": 0, "context": "All 4 tasks implemented, tests pass"},
  {"step": "review",           "iteration": 0, "context": "2 unresolved comments on src/lib.rs"},
  {"step": "address_reviews",  "iteration": 1, "context": "Fixed comment on line 42; line 87 still failing"},
  {"step": "review",           "iteration": 1, "context": "1 unresolved comment remains on src/lib.rs:87"}
]
```

The engine appends a new entry after each step completes. Inside a `while` loop, entries from all iterations are included, making it possible for an agent to detect repeated failures on the same issue.

---

## Gates and checkpoints

Gates pause workflow execution until an external condition is met. The run enters `waiting` status.

**Human gates** — require explicit action through conductor.

```
gate human_approval {
  prompt     = "Review PLAN.md before implementation begins."
  timeout    = "24h"
  on_timeout = fail
}

gate human_review {
  prompt     = "Review agent findings. Add notes if needed."
  timeout    = "48h"
  on_timeout = continue
}
```

`human_review` accepts optional written feedback. That text is injected into the next step's prompt as `{{gate_feedback}}`, allowing a human to redirect the next agent without modifying any files.

**Automated gates** — poll an external signal. No human action inside conductor required.

```
gate pr_approval {
  min_approvals = 1
  timeout       = "72h"
  on_timeout    = fail
}

gate pr_checks {
  timeout    = "2h"
  on_timeout = fail
}
```

**Gate CLI commands:**
```
conductor workflow gate-approve  <run-id>           # approve a waiting human gate
conductor workflow gate-reject   <run-id>           # reject (fails the workflow)
conductor workflow gate-feedback <run-id> "<text>"  # provide feedback and approve
```

---

## Error handling

**Per-step retry and fallback:**

```
call implement {
  retries = 2
  on_fail = diagnose
}
```

The engine retries the step up to `retries` times on failure. If all retries are exhausted and `on_fail` is set, that agent is called once before the workflow fails. The `on_fail` agent automatically receives these additional template variables:

| Variable | Value |
|---|---|
| `{{failed_step}}` | Name of the step that failed |
| `{{failure_reason}}` | Error text from the last failed run |
| `{{retry_count}}` | Number of retries attempted |
| `{{prior_context}}` | Context from the step before the failing step |

**Always block** — runs regardless of workflow outcome:

```
always {
  call notify_result
}
```

`always` steps receive `{{workflow_status}}` (`"completed"` or `"failed"`) in their prompt. `always` steps themselves do not retry and do not block workflow status — a failure in an `always` step is logged but does not change the workflow's terminal status.

---

## Dry-run mode

Invoked with `conductor workflow run <name> --dry-run`. Behavior per construct:

| Construct | Dry-run behavior |
|---|---|
| `call` with `can_commit = false` | Runs normally |
| `call` with `can_commit = true` | Prepends "DO NOT commit or push any changes" to the agent prompt |
| `gate human_approval` / `human_review` | Auto-approved; `{{gate_feedback}}` is empty |
| `gate pr_approval` / `pr_checks` | Skipped (treated as satisfied) |
| `parallel` | All agents run with the same `can_commit` rule above |
| `always` | Runs normally; receives `{{workflow_status}}` as usual |

Dry-run is stored on `workflow_runs.dry_run` so run history clearly identifies dry runs.

---

## Workflow snapshot

When a workflow run is created, the parsed `WorkflowDef` is serialized to JSON and stored in `workflow_runs.definition_snapshot`. The engine always resumes from the snapshot — never re-parses the `.wf` file. This ensures that editing a workflow file mid-run does not change the behavior of an in-flight execution.

---

## `trigger` field (v1 scope)

`trigger = "manual"` is the only implemented value in v1. Conductor has no daemon and no event listener, so `"pr"` and `"scheduled"` triggers cannot fire automatically. These values are accepted by the parser and stored, but are reserved for v2. Setting a non-manual trigger in v1 has no effect and the parser will emit a warning.

---

## Engine changes

### Parser (`workflow_config.rs` → `workflow_dsl.rs`)

A hand-written recursive descent parser converts `.wf` files into a `WorkflowDef` with a tree-structured body:

```
WorkflowNode::Call     { agent: String, retries: u32, on_fail: Option<String> }
WorkflowNode::If       { step: String, marker: String, body: Vec<WorkflowNode> }
WorkflowNode::While    { step: String, marker: String, max_iter: u32,
                         stuck_after: Option<u32>, on_max_iter: OnMaxIter,
                         body: Vec<WorkflowNode> }
WorkflowNode::Parallel { fail_fast: bool, min_success: Option<u32>,
                         calls: Vec<String> }
WorkflowNode::Gate     { gate_type: GateType, prompt: Option<String>,
                         min_approvals: u32, timeout: Duration,
                         on_timeout: OnTimeout }
WorkflowNode::Always   { body: Vec<WorkflowNode> }
```

### Execution (`workflow.rs`)

Replace the linear `for` loop with a recursive `execute_nodes()` that:

- Snapshots `WorkflowDef` to JSON in `workflow_runs.definition_snapshot` at run start
- Tracks a monotonically increasing `position` counter across all iterations and constructs
- Appends the `CONDUCTOR_OUTPUT` instruction to every agent prompt before dispatch
- Parses the `<<<CONDUCTOR_OUTPUT>>>` block from each completed agent run
- Accumulates a `prior_contexts` array and injects both `{{prior_context}}` and `{{prior_contexts}}` into each subsequent prompt
- Substitutes `inputs` values and gate/error variables into prompts before dispatch
- Handles `while` loops with iteration counter, stuck detection (identical marker set for N iterations), and `on_max_iter` policy
- Handles `parallel` groups by spawning all calls concurrently, polling until completion, applying `fail_fast` / `min_success` policy, then merging marker sets
- Enforces per-step `retries`, calls `on_fail` agent with failure context if exhausted
- For human gates: writes `waiting` step record, polls DB until `gate_approved_at` is set or timeout expires
- For automated gates: polls `gh pr view` on standard interval until condition is met or timeout expires
- Runs `always` body after main body regardless of outcome; does not alter terminal workflow status on `always` failure
- Is fully resumable from DB state: on startup, scans for `workflow_runs` in `running` or `waiting` status and re-enters `execute_nodes()` from the last non-terminal step record using the stored `definition_snapshot`
- Supports `--dry-run` mode per the behavior table above

### DB schema (new migration)

```sql
-- workflow_runs additions
ALTER TABLE workflow_runs ADD COLUMN definition_snapshot TEXT; -- serialized WorkflowDef JSON

-- workflow_run_steps additions
ALTER TABLE workflow_run_steps ADD COLUMN iteration         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflow_run_steps ADD COLUMN parallel_group_id TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN context_out       TEXT;   -- CONDUCTOR_OUTPUT context field
ALTER TABLE workflow_run_steps ADD COLUMN markers_out       TEXT;   -- JSON array of emitted markers
ALTER TABLE workflow_run_steps ADD COLUMN retry_count       INTEGER NOT NULL DEFAULT 0;

-- Gate columns
ALTER TABLE workflow_run_steps ADD COLUMN gate_type         TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_prompt       TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_timeout      TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_approved_by  TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_approved_at  TEXT;
ALTER TABLE workflow_run_steps ADD COLUMN gate_feedback     TEXT;
```

The `status` CHECK constraint on both `workflow_runs` and `workflow_run_steps` gains a new value: `'waiting'`.

---

## Management features

**CLI:**
```
conductor workflow list                           # name, trigger, step count
conductor workflow show <name>                    # ASCII step graph with loop/parallel/gate annotations
conductor workflow validate <name>                # check all agents exist, inputs are declared,
                                                  #   warn on non-manual triggers
conductor workflow run <name> [--input k=v]       # run the workflow
              [--dry-run]
conductor workflow cancel <run-id>                # cancel a running or waiting workflow
conductor workflow runs [--worktree id]           # history; shows waiting/running/dry-run status
conductor workflow run-show <run-id>              # per-step detail: iteration, markers, gate state,
                                                  #   retry count, cost, turns
conductor workflow gate-approve  <run-id>         # approve a pending human gate
conductor workflow gate-reject   <run-id>         # reject (fails the workflow)
conductor workflow gate-feedback <run-id> "<text>"# provide feedback and approve
```

**TUI:** Workflows tab with list view and step detail pane showing per-step status, iteration count, markers emitted, retry count, cost, and turns. Runs blocked on a human gate show a "waiting for approval" state with the gate prompt visible and inline approve/reject/feedback actions.

**Web UI:** `/workflows` list page + `/workflows/<name>` detail page with step visualization showing parallel branches, loop indicators, and gate nodes. Pending human gates show an approval form inline.

---

## Implementation plan

1. Write recursive descent parser for `.wf` DSL into `WorkflowNode` AST (`workflow_dsl.rs`)
2. Implement `<<<CONDUCTOR_OUTPUT>>>` block emission (prompt template) and JSON parsing (result handler)
3. Update execution engine to walk AST recursively with input substitution and `prior_contexts` accumulation
4. Add workflow snapshot: serialize `WorkflowDef` to `workflow_runs.definition_snapshot` at run start; resume always reads from snapshot
5. Add parallel step group support to engine with `fail_fast` / `min_success` policies
6. Add per-step retry + `on_fail` agent with failure context variables
7. Add `[[always]]` support with `{{workflow_status}}` injection
8. Add gate support: `waiting` status, human gate DB polling, automated gate `gh` polling
9. Make engine resumable from DB state (re-enter from last non-terminal step using snapshot)
10. Add `--dry-run` mode with per-construct behavior
11. Add all DB columns via a single migration (iterations, parallel, context, markers, retries, gates, snapshot)
12. Update `status` CHECK constraints for `waiting` on both tables
13. Add `conductor workflow list/show/validate/run/cancel/runs/run-show/gate-*` CLI commands
14. Add Workflows tab to TUI with gate approval actions
15. Port existing workflow files to new `.wf` format
16. Add Web UI list + detail pages with inline gate approval form
