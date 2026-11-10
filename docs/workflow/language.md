# Workflow Language Reference

The semantics of the `.wf` language, organised by construct. Each rule has a
one-line example and the engine behaviour behind it. Authors and reviewers of
workflows, scripts, agent briefs and templates should treat this as the place
to check a rule before relying on it.

The engine is [runkon-flow](https://github.com/runkon/runkon), consumed from
crates.io, so the source of truth for behaviour is that crate (version pinned
in the root `Cargo.toml`). The paths below, such as `executors/script.rs`, are
inside it. [engine.md](engine.md) covers design rationale and grammar;
[structured-agent-output.md](structured-agent-output.md) covers schemas in
depth. This document covers the rules that are easy to get wrong. The
`review-workflow-semantics` agent brief (`.conductor/agents/review-workflow-semantics.md`)
carries a short copy of the rules a reviewer checks; **when a rule here
changes, change the brief in the same PR.**

Contents: [Workflow file](#workflow-file) · [Steps and names](#steps-and-names) ·
[call](#call) · [call workflow](#call-workflow) · [script](#script) ·
[gate](#gate) · [if / unless](#if--unless) · [Loops](#loops) · [do](#do) ·
[parallel](#parallel) · [foreach](#foreach) · [always](#always) ·
[output and schemas](#output-and-schemas) · [Markers](#markers) ·
[FLOW_OUTPUT](#flow_output) · [Template variables](#template-variables) ·
[Resume](#resume) · [Dry run](#dry-run) · [Checklist](#checklist)

---

## Workflow file

```
workflow iterate-pr {
  meta {
    title       = "Iterate on PR Feedback"   // display name
    description = "..."
    trigger     = "manual"                    // manual | pr | scheduled
    targets     = ["worktree"]                // required: worktree and/or repo
    group       = "Development"               // UI grouping
  }
  inputs {
    pr_url    required
    auto_merge boolean                         // absent = "false", never required
    force_review default = "false"
  }
  ...nodes...
}
```

- `targets` is required. `title`, `description`, `trigger` and `group` are optional.
- Comments are `//` or `#` to end of line.
- Input values are strings. A `boolean` input is the string `"true"` or `"false"`; only a case-insensitive `"true"` counts as true.
- Identifiers allow `[a-zA-Z0-9_-]`, so `push-and-pr` is one name.

## Steps and names

Every `call`, `script`, `call workflow` and `gate` is a **step**. A step's
**name** is its identifier (for a `gate`, its type; for a path-based agent
reference, the file stem).

- Conditions, `prior_contexts`, `foreach` options and resume all key on the name. See [if / unless](#if--unless) and [Resume](#resume).
- Give every step in a workflow a unique name. A repeated name inside one iteration is ambiguous for conditions (`if fmt-fix.changed` reads only the latest) and ties resume to occurrence order.
- The exception is a deliberate same-name retry: re-running `verify` under `if verify.has_failures` so that a later condition reads the retry's result.

## call

Runs one agent (`.conductor/agents/<name>.md`) to completion.

```
call implement { retries = 2  on_fail = diagnose  output = "task-plan"  with = ["review-diff-scope"] }
```

| Option | Meaning |
|---|---|
| `retries = N` | Retry after failure, N more attempts (default 0) |
| `on_fail = <agent>` | Run this agent if retries are exhausted. It gets `{{failed_step}}`, `{{failure_reason}}`, `{{retry_count}}`. `on_fail = continue` skips the failed step without failing the run |
| `output = "<schema>"` | Bind an output schema (see [output and schemas](#output-and-schemas)) |
| `with = [..]` | Prompt snippets from `.conductor/prompts/`, appended after variable substitution |
| `as = "<bot>"` | Run under a named GitHub App identity (`[github.apps.<name>]`); `GH_TOKEN` is injected |
| `timeout = "5m"` | Cancel the step after this duration |
| `max_turns = N` | Host-enforced turn cap |
| `require_output = bool` | Fail the step when the agent ends without a parseable FLOW_OUTPUT. Default `true` when an `output` schema is bound, else `false` |

- An agent that omits FLOW_OUTPUT records no markers and an empty context. That is not an error unless `require_output` is in effect. Behind it: `executors/call.rs`.
- `call a { as = "reviewer" }` makes every `gh` call in that agent run as the bot.

## call workflow

Runs another workflow as one opaque step. Written `call workflow <name>`.

```
call workflow test-coverage { inputs { pr_url = "{{pr_url}}" } }
```

- The child needs every `required` input; its `default`s fill the rest. Values support `{{variable}}`.
- The child's **final** step's markers and context become the step's output in the parent: `if rebase-worktree.pulled_new_commits` reads the child's last markers, under the step name `rebase-worktree`.
- A child failure fails the parent step; `retries` re-runs the whole child, then `on_fail` applies.
- A gate inside a child blocks the parent, shown as a gate on the parent.
- Depth limit is 5; a circular reference fails validation. Behind it: `executors/call_workflow.rs`.
- The step's name is the child workflow's name. Two `call workflow review-pr` in one iteration collide (runkon/runkon#108, conductor #3118).

## script

Runs a shell command with no model call.

```
script review-scope {
  run     = ".conductor/scripts/review-scope.sh"
  env     = { PR_NUMBER = "{{pr_number}}", PRIOR_OUTPUT = "{{prior_output}}" }
  timeout = 600
}
```

| Option | Meaning |
|---|---|
| `run` | Script path, `{{variable}}` allowed. Resolved against the worktree, then the repo, then `~/.claude/skills/` |
| `env = { K = "v" }` | Environment for the script; values substitute `{{variable}}` from the raw (not shell-quoted) variable map. Security-sensitive names are ignored |
| `timeout = N` | **Seconds** (a number, unlike a `call`'s `"5m"`). The script is killed and the step is `TimedOut` |
| `retries`, `on_fail` | As for `call`; `on_fail` may be an agent or `continue` |
| `as = "<bot>"` | Injects that identity's token as `GH_TOKEN` |

Rules, from `executors/script.rs` and `prompt_builder.rs`:

- The script runs as `sh -c` in the worktree with stdin closed. Anything it needs must arrive through `env`.
- The script emits its own FLOW_OUTPUT block on stdout. **A script step gets no schema-derived markers**; nothing derives them from fields. A script that wants a marker prints it in `markers`. A `.conductor/schemas/*.yaml` named after a script's output documents its contract only (for example `review-aggregator.yaml` for `aggregate-reviews.sh`); renaming a field there changes nothing at run time.
- Without a FLOW_OUTPUT block, the step has no markers and its context is stdout truncated to 2000 characters.
- With a block that does not parse (invalid JSON, a missing end marker, the one-line form `<<<FLOW_OUTPUT>>> {...}`), the **script step fails**. Agents are more lenient (see [FLOW_OUTPUT](#flow_output)).
- Extra top-level string fields in the block become `{{name}}` variables for later steps: `{"markers": [], "context": "", "review_scope": "full"}` makes `{{review_scope}}`. A name the engine already injects (`ticket_id`, `workflow_run_id` and the other [injected variables](#template-variables)) cannot be overwritten. Exported variables persist across loop iterations, so every branch of a script should emit the field, or a stale value from the previous iteration is read.
- A non-zero exit fails the step.

## gate

**The word after `gate` is the gate type, not a name.** It also becomes the
step's name.

```
gate human_review { prompt = "Approve the plan." timeout = "72h" on_timeout = fail }
```

`gate human_plan_decisions { ... }` fails every run at start with `no registered GateResolver for type …`, and `conductor workflow validate` doesn't catch it yet (#3284; the incident was #3281).

conductor registers four types:

| Type | Waits for | Options |
|---|---|---|
| `human_approval` | A person to approve or reject (CLI, TUI, web) | `prompt` |
| `human_review` | The same, with written feedback | `prompt`, `options = step.field` |
| `pr_checks` | CI checks on the PR to pass | `timeout`, `on_timeout` |
| `pr_approval` | Approvals on the PR | `min_approvals = N` (default 1), or `mode = "review_decision"` for GitHub's branch-protection-aware `reviewDecision == APPROVED` |

- `timeout` is a duration string (`"2h"`). `on_timeout` is `fail` (default) or `continue`. Every gate should set both on purpose.
- Feedback entered on approval reaches the **next** step as `{{gate_feedback}}`, empty when none. It is stored on the gate's step row, so it survives a resume.
- `options = analyze-mobile-ux.findings` takes a `step.field` reference to a structured-output list and offers it to the approver as choices. The step must already have a result, or the gate fails before it is created.
- A rejected gate fails the run.
- In a [dry run](#dry-run) human gates are auto-approved and automated gates are skipped.
- The step name is the type, so two `gate pr_checks` in one workflow share a name.

## if / unless

```
if review-aggregator.has_review_issues { call address-reviews }
unless verify.has_failures { call push-and-pr }
if auto_merge { gate pr_checks { timeout = "2h" } }
```

- A condition is `step.marker` or a bare boolean input. There is **no `&&`, `||`, `!` or comparison**. `unless` is the exact inverse of `if`.
- A `step.marker` condition reads **the latest result stored under that step name**, whatever its iteration. In a loop iteration where the step was skipped (by its own `if`/`unless`, or because its branch was not taken) the condition sees the previous iteration's markers. A step that has never run reads as false.
- Therefore a condition should name a step that runs earlier in the same iteration on every path that reaches the condition.
- The marker must be emitted by that step: the agent's FLOW_OUTPUT, the script's FLOW_OUTPUT or a schema's `markers:` (see [Markers](#markers)). A typo reads as false and never errors.
- A `call workflow` step's markers are the child's final step's markers.

Behind it: `executors/control_flow.rs`, `eval_condition`. A `step.marker` condition checks the `markers` vec in the step's latest result; a bare input does a case-insensitive `"true"` match.

## Loops

```
while review-aggregator.has_review_issues { max_iterations = 3  stuck_after = 2  on_max_iter = fail  ... }
do { max_iterations = 5  ... call verify } while verify.has_failures
```

| Option | Meaning |
|---|---|
| `max_iterations` | **Required.** Hard cap |
| `stuck_after = N` | Optional. Fail if the condition's step's marker set is identical for N consecutive iterations |
| `on_max_iter` | `fail` (default) or `continue` when the cap is reached |

- `while` evaluates its condition before each iteration; `do {} while` always runs once and evaluates after.
- The condition obeys the latest-result rule above. In `do { ... } while verify.has_failures` the condition reads `verify` from the iteration that just ended, so `verify` must run in every iteration.
- A loop needs an exit marker that something can actually emit.
- `prior_contexts` includes entries from every iteration.
- A `do` without `while` is a [grouping block](#do), not a loop.

## do

A sequential grouping block. Any node type may appear inside.

```
do { output = "review-findings"  with = ["review-diff-scope"]  call review-security  call review-style }
```

- Block-level `output` and `with` apply to every `call` inside; a call's own `output` replaces the block's, and its `with` list is appended after the block's.
- Blocks nest; the outer values are restored on exit.

## parallel

Runs `call`s concurrently in the same worktree, so they must be read-only or touch disjoint files.

```
parallel { output = "review-findings"  fail_fast = false  call review-security  call review-db-migrations { if = "detect-db-migrations.has_db_migrations" } }
```

| Option | Meaning |
|---|---|
| `fail_fast` | Default `true`: cancel the rest when one fails |
| `min_success = N` | Minimum successes required; default all |
| `output`, `with` | Block-level, as in [do](#do) |
| per-call `if = "step.marker"` | Run that call only if the marker is set; otherwise it is recorded `skipped` |

- The block's markers are the **union** of every completed call's markers; skipped calls add none and count as successes.
- Only `call` nodes are allowed inside; use `do` or separate steps for scripts.
- Skipped steps are re-evaluated on resume; they are not treated as completed.

## foreach

Runs a child workflow once per item.

```
foreach sprint-work { over = tickets  scope = { ticket_id = "{{inputs.root_ticket_id}}" }  ordered = true  max_parallel = 3  workflow = "ticket-to-pr"  inputs = { ticket_id = "{{item.id}}" }  on_child_fail = skip_dependents }
```

- Required: `over` (`tickets`, `repos`, `workflow_runs`), `max_parallel`, `workflow`.
- Optional: `scope`, `filter`, `ordered`, `on_cycle` (`fail` or `warn`), `inputs`, `on_child_fail` (`halt`, `continue`, `skip_dependents`).
- `{{item.*}}` is substituted at dispatch time, not at parse time.
- Items are tracked in `workflow_run_step_fan_out_items`, so a resume does not re-dispatch an item already present.
- One child workflow means one ticket per worktree for `ticket-to-pr`; see engine.md#foreach for the item fields and ordering rules.

## always

Runs after the body whatever its outcome. It sees `{{workflow_status}}` (`completed` or `failed`). A failure inside `always` is logged but does not change the run's status.

```
always { call notify-result }
```

## output and schemas

`output = "name"` on a `call` (or a `parallel`/`do` block) binds `.conductor/schemas/name.yaml`.

- Schemas are looked up in `.conductor/workflows/<workflow>/schemas/` first, then `.conductor/schemas/`, worktree before repo.
- The engine injects the schema into the agent prompt, parses and validates the agent's FLOW_OUTPUT against it, and exposes the parsed JSON as `{{prior_output}}` to the next step.
- The schema's `summary` field, if present, is used as the step's `context`.
- A schema file's `markers:` section derives markers from fields:

```yaml
markers:
  has_review_issues: "overall_approved == false"
  has_blocking_findings: "blocking_findings.length > 0"
```

- **These apply to agent steps.** A script step gets none ([script](#script)).
- Renaming or removing a field makes its marker silently stop firing; every `if` reading it then reads false.
- A schema bound to a step makes `require_output` default to true, so an agent that doesn't produce a parseable block fails the step.

## Markers

A marker is a string in a step's `markers` array. It is the only thing `if`, `unless`, `while` and `do {} while` test.

- Sources: the `markers` array in the step's FLOW_OUTPUT; a schema's `markers:` rules (agents only); the union of a `parallel` block; a child workflow's last step.
- A marker not emitted by any step is read as false, not as an error. Grep the producer when you add a condition.
- Text in an agent's prose never counts. The engine does not scan output text.
- A condition only sees the latest result per step name ([if / unless](#if--unless)).

## FLOW_OUTPUT

The block a step prints to hand markers, context and extra fields to the engine.

```
<<<FLOW_OUTPUT>>>
{"markers": ["has_review_issues"], "context": "Found 3 issues"}
<<<END_FLOW_OUTPUT>>>
```

- **The last block in stdout wins.** An earlier block, or one inside a quoted example, is ignored once a later one exists.
- The start marker must begin a line (optional leading whitespace) and be followed by `{`, `[` or a code fence on the next line. The end marker must also begin a line. A marker mentioned mid-line in prose, a shell command or a JSON string is ignored.
- Use the three-line form. The one-line form `<<<FLOW_OUTPUT>>> {...}` with no end marker is not parsed.
- A code fence around the JSON is tolerated; trailing commas and some bad escapes are repaired.
- Invalid JSON: for an **agent** step it yields no markers and is logged, not failed (unless `require_output` is in effect); for a **script** step the step fails.
- `markers` is a string array, `context` a string; other top-level string fields are extras (see [script](#script)).
- Build script output with `jq -n --arg` rather than interpolating into a heredoc, since quotes and newlines in `"context": "$var"` break the JSON.
- Behind it: `helpers.rs::parse_flow_output`, `find_flow_output_block`.

## Template variables

`{{name}}` is substituted in agent prompts, in `script` `run` and `env` values, in `call workflow` `inputs`, and in `foreach` `inputs`. An unknown name is left as the literal text `{{name}}`.

| Variable | Holds |
|---|---|
| `{{prior_context}}` | **Only** the `context` string of the most recent step, usually a one- or two-sentence summary. Structured fields (`missing[]`, `findings[]`) are **not** in it (#3280) |
| `{{prior_output}}` | The structured JSON of the most recent completed step that has one, whatever its name; **not a lookup by name**. For a schema'd agent, its parsed output; for a script, its whole FLOW_OUTPUT object. Absent until a step produces one |
| `{{prior_output_file}}` | Path to a temp file holding the last script step's stdout |
| `{{prior_contexts}}` | JSON array `[{"step","iteration","context"}, ...]` of every completed step in run order. Repeated names are all included, never deduped, also when replayed on resume. A script picking `first` or `last` for a name sees every occurrence |
| `{{gate_feedback}}` | The feedback typed at the last gate, empty if none or after no gate; persisted for resume |
| `{{dry_run}}` | `"true"` or `"false"` |
| `{{workflow_run_id}}` | ULID of the run |
| `{{workflow_status}}` | In `always`: `completed` or `failed` |
| `{{failed_step}}`, `{{failure_reason}}`, `{{retry_count}}` | In an `on_fail` agent only |
| `{{ticket_id}}`, `{{ticket_title}}`, `{{ticket_body}}`, `{{ticket_url}}`, `{{ticket_source_id}}`, `{{ticket_source_type}}`, `{{ticket_raw_json}}` | The run's target ticket |
| `{{repo_id}}`, `{{repo_name}}`, `{{repo_path}}` | The registered repo |
| `{{feature_base_branch}}` | The branch the PR targets for this run |
| `{{<input>}}` | Any declared input, or one the run was started with |
| `{{<export>}}` | A string extra from an earlier script's FLOW_OUTPUT |
| `{{item.*}}`, `{{inputs.*}}` | In `foreach` dispatch only |

- A workflow input of the same name overrides an injected default; a script export cannot override an injected key.
- `prior_context`/`prior_output` are about *the previous step*, which in a loop or after a skipped step may not be the one you mean. Read a specific step through `prior_contexts` or have the step write a file.
- A script reads `{{prior_output}}` through `env = { PRIOR_OUTPUT = "{{prior_output}}" }`; it isn't set in the environment otherwise.
- Don't write `{{variable}}` inside instructional prose of an agent brief ("if `{{gate_feedback}}` is present…"); after substitution the sentence breaks. Use plain English and one dedicated section containing the variable.

## Resume

`conductor workflow resume <run>` (and `conductor_resume_run`) re-enters the saved definition snapshot.

- Completed steps are recorded by `(name, iteration)` and are skipped, their markers, context, cost and `gate_feedback` restored from the database. The n-th occurrence of a name matches the n-th saved row (runkon 0.7.2 and later), so a repeated name no longer skips the wrong step, though it still makes conditions ambiguous.
- Steps skipped by a condition are re-evaluated on resume, not treated as completed.
- A step that failed, or was running, runs again. A step whose effect (push, PR create, comment, commit) happened but was not recorded complete **runs again**: make those effects idempotent.
- `--from-step NAME` resets NAME and every later step. `--restart` clears everything.
- Running subprocesses of the old run are terminated on resume.
- The snapshot is the definition at run start, so editing the `.wf` does not change a run in flight.
- `fail-verify.sh` tells the user to resume with `--from-step verify`, one reason a same-name retry of `verify` must keep its name.

## Dry run

`conductor workflow run <name> --dry-run`, or `conductor workflow validate --dry-run <name>` for a recorded smoke run.

- Human gates auto-approve with empty `{{gate_feedback}}`; automated gates are skipped.
- A `call` with `can_commit = true` is told not to commit or push; `{{dry_run}}` is `"true"` for every agent and script, so side effects should check `DRY_RUN`.
- `always` runs normally.
- `validate --dry-run` skips every script step, auto-approves gates and cancels at the first agent step.

## Checklist

For a change to a `.wf` file or template:

1. The word after `gate` is one of `human_approval`, `human_review`, `pr_checks`, `pr_approval`.
2. Every condition names a step that ran earlier in the same iteration, and the marker has a producer.
3. No `&&`, `||`, `!` or comparisons.
4. Every loop has `max_iterations` and an exit marker that can be emitted.
5. Step names are unique, or the repetition is a deliberate retry.
6. Structured data is read from `prior_output`, not `prior_context`.
7. Script steps print their own markers; no schema marker is assumed for them.
8. FLOW_OUTPUT is three lines, valid JSON, the last block printed.
9. Gates set `timeout` and `on_timeout`.
10. Side effects honour `DRY_RUN` and are safe to repeat on resume.
11. `conductor workflow validate --dry-run <name>` passes for each changed workflow.
