# Structured Agent Output

This document describes an optional structured output system for workflow
agents. It builds on the existing `CONDUCTOR_OUTPUT` mechanism by allowing
workflows to define output schemas that the engine injects into agent prompts,
then parses, validates, and acts on programmatically.

---

## Motivation

The current `CONDUCTOR_OUTPUT` block serves two purposes:

1. **Control flow** — markers drive `if`/`while` conditions
2. **Context threading** — a free-text summary passed to the next step

This works well for orchestration, but some agent types produce output that is
inherently structured — reviewer findings, test coverage reports, lint results,
plans with discrete tasks. Today that structure lives in free-text prose, and
any downstream consumption (PR comments, aggregation, threshold checks) must
parse it heuristically or not at all.

Structured output lets the engine understand agent results at a field level,
enabling:

- Consistent formatting of PR comments across reviewers
- Programmatic filtering, sorting, and deduplication of findings
- Threshold-based decisions (e.g., block merge if any `high` severity finding)
- Cross-agent aggregation (combine findings from parallel reviewers)
- Machine-readable data for dashboards and analytics

---

## Design

### Key decision: schemas belong to the workflow, not the agent

An output schema describes how the **workflow consumes** an agent's result, not
a property of the agent itself. A security reviewer might be used in one
workflow that wants structured findings and another that just wants
markers + context. Embedding the schema in the agent file would force every
consumer to accept the same output shape and require duplicating the schema
across every reviewer agent.

Instead, schemas are:

1. Defined in standalone files under `.conductor/schemas/`
2. Referenced from `call` sites in the workflow

This separates concerns cleanly:

- **Agent files** — define *who the agent is* (role, permissions, prompt)
- **Schema files** — define *what shape the output takes*
- **Workflow files** — bind agents to schemas at the call site

### Schema files

Schemas live in `.conductor/schemas/<name>.yaml`:

```yaml
# .conductor/schemas/review-findings.yaml
fields:
  findings:
    type: array
    items:
      file: string
      line: number
      severity: enum(critical, high, medium, low, info)
      category:
        type: string
        desc: "OWASP category or general area"
        examples: ["injection", "auth", "config", "cryptography"]
      message: string
      suggestion?:
        type: string
        desc: "Suggested fix or remediation"
  approved: boolean
  summary: string
```

The schema file is pure structure — no prompt text, no agent configuration.

### Referencing schemas from workflows

The `output` option on `call` binds a schema to an agent invocation:

```
call review-security { output = "review-findings" }
call review-style    { output = "review-findings" }
call review-perf     { output = "review-findings" }
```

The engine resolves the name to `.conductor/schemas/review-findings.yaml`,
loads the schema, and uses it for prompt injection and output parsing.

For `parallel` blocks, the `output` option can be set once for all agents:

```
parallel {
  output      = "review-findings"
  fail_fast   = false
  min_success = 1

  call review-security
  call review-style
  call review-perf
}
```

Individual calls within a `parallel` block can override the block-level output:

```
parallel {
  output = "review-findings"

  call review-security
  call review-style
  call lint-check { output = "lint-results" }
}
```

When no `output` is specified, the default `CONDUCTOR_OUTPUT` markers + context
behavior applies. No breaking change.

### Schema resolution

Schema names are resolved by checking these locations in order:

| Priority | Path |
|---|---|
| 1 | `.conductor/workflows/<workflow>/schemas/<name>.yaml` |
| 2 | `.conductor/schemas/<name>.yaml` |

This follows the same pattern as agent resolution — workflow-local overrides,
then shared. Each priority is checked in the worktree path first, then the
repo path.

Explicit paths (quoted strings) are also supported, following the same rules
as agent path resolution:

```
call review-security { output = "./custom/schemas/my-review.yaml" }
```

---

## Prompt injection

When a `call` has an `output` schema, the engine generates JSON output
instructions from the schema and appends them to the agent's prompt (replacing
the generic `CONDUCTOR_OUTPUT` instructions).

For the `review-findings` schema, the appended instructions would be:

```
When you have finished your work, output the following block exactly as the
last thing in your response. Do not include this block in code examples or
anywhere else — only as the final output.

<<<CONDUCTOR_OUTPUT>>>
{
  "findings": [
    {
      "file": "path/to/file.rs",
      "line": 42,
      "severity": "critical|high|medium|low|info",
      "category": "description of finding category",
      "message": "description of the finding",
      "suggestion": "optional suggestion for fixing"
    }
  ],
  "approved": true,
  "summary": "one or two sentence summary of your review"
}
<<<END_CONDUCTOR_OUTPUT>>>

The "findings" array should contain one entry per issue found. If there are
no issues, return an empty array and set "approved" to true.
"suggestion" is optional and may be omitted.
```

The delimiters remain `<<<CONDUCTOR_OUTPUT>>>` / `<<<END_CONDUCTOR_OUTPUT>>>`
for consistency. The only change is the shape of the JSON between them.

---

## Backward compatibility with markers

Structured output replaces markers for agents that use it, but the engine
needs to bridge the two systems so `if`/`while` conditions still work.

The engine auto-derives markers from structured output using simple rules:

| Schema field | Derived marker |
|---|---|
| `approved: false` | `not_approved` |
| `findings` array is non-empty | `has_findings` |
| Any finding with `severity: critical` | `has_critical_findings` |
| Any finding with `severity: high` | `has_high_findings` |

These derived markers are available to downstream `if`/`while` conditions
just like manually emitted markers. The agent does not need to emit them
explicitly.

Custom marker derivation rules can be defined per-schema (see
[Marker derivation rules](#marker-derivation-rules)).

The `summary` field (if present in the schema) is used as the `context` value
for `{{prior_context}}` threading.

---

## Schema types

The schema definition supports these types:

| Type | Description | JSON equivalent |
|---|---|---|
| `string` | Free-text string | `"..."` |
| `number` | Integer or float | `42`, `3.14` |
| `boolean` | True/false | `true`, `false` |
| `enum(a, b, c)` | One of the listed values | `"a"` |
| `array` | List of items (with `items` sub-schema) | `[...]` |
| `object` | Nested structure (with named fields) | `{...}` |

### Field definitions

Fields can be declared in short form (just the type) or object form (type plus
metadata). Use short form for self-explanatory fields; use object form when
the agent needs additional guidance.

**Short form** — type only:

```yaml
file: string
line: number
approved: boolean
```

**Object form** — type plus metadata:

```yaml
category:
  type: string
  desc: "OWASP category or general area"
  examples: ["injection", "auth", "config", "cryptography"]
```

| Property | Required | Description |
|---|---|---|
| `type` | yes | One of the supported types |
| `desc` | no | Human-readable description, included in the prompt to guide the agent |
| `examples` | no | Sample values included in the prompt to improve output consistency |

Both forms can be mixed freely within a schema. Fields are required by default.
Optional fields are marked with `?` on the field name:

```yaml
fields:
  findings:
    type: array
    items:
      file: string
      line: number
      severity: enum(critical, high, medium, low, info)
      category:
        type: string
        desc: "OWASP category or general area"
        examples: ["injection", "auth", "config", "cryptography"]
      message: string
      suggestion?:
        type: string
        desc: "Suggested fix or remediation"
  approved: boolean
  summary: string
```

When the engine generates prompt instructions, it includes `desc` and
`examples` as inline hints alongside each field. Fields without metadata
show only their name and type.

---

## Marker derivation rules

Schemas can declare explicit rules for deriving markers from structured fields:

```yaml
# .conductor/schemas/review-findings.yaml
fields:
  findings:
    type: array
    items:
      file: string
      line: number
      severity: enum(critical, high, medium, low, info)
      category:
        type: string
        desc: "OWASP category or general area"
        examples: ["injection", "auth", "config", "cryptography"]
      message: string
      suggestion?:
        type: string
        desc: "Suggested fix or remediation"
  approved: boolean
  summary: string

markers:
  has_findings: "findings.length > 0"
  has_critical_findings: "findings[severity == critical].length > 0"
  has_high_findings: "findings[severity == high].length > 0"
  not_approved: "approved == false"
```

When `markers` is declared, only the listed rules apply — no implicit
derivation. When `markers` is omitted, the engine applies built-in defaults
based on field names and types.

The expression language is deliberately minimal — field access, equality
checks, array length, and array filtering. This is not a general-purpose
expression evaluator.

---

## Use cases

### Reviewer agents

The primary motivation. Three reviewer agents, one schema:

```
# .conductor/schemas/review-findings.yaml
# (defined once, used by all reviewers)

parallel {
  output      = "review-findings"
  fail_fast   = false
  min_success = 1

  call review-security
  call review-style
  call review-perf
}

if review-security.has_critical_findings {
  call escalate
}
```

The engine can:

- **Format PR comments consistently** — table of findings sorted by severity,
  grouped by file, with line links
- **Aggregate across reviewers** — deduplicate findings on the same file:line
  from different reviewers
- **Apply thresholds** — block auto-merge if any `critical` or `high` finding
- **Track metrics** — finding counts by category and severity over time

### Coverage analysis

```yaml
# .conductor/schemas/coverage-report.yaml
fields:
  coverage_percent:
    type: number
    desc: "Overall line coverage as a percentage (0-100)"
  uncovered_files:
    type: array
    items:
      file: string
      uncovered_lines: array
      reason?:
        type: string
        desc: "Why this file lacks coverage"
        examples: ["new file with no tests", "complex branching logic", "error handling paths"]
  summary: string

markers:
  has_missing_tests: "uncovered_files.length > 0"
  low_coverage: "coverage_percent < 80"
```

### Lint analysis

```yaml
# .conductor/schemas/lint-results.yaml
fields:
  errors:
    type: array
    items:
      file: string
      line: number
      rule: string
      message: string
      fixable: boolean
  error_count: number
  fixable_count: number
  summary: string

markers:
  has_lint_errors: "error_count > 0"
  has_fixable_errors: "fixable_count > 0"
```

### Planning

```yaml
# .conductor/schemas/task-plan.yaml
fields:
  tasks:
    type: array
    items:
      id:
        type: string
        desc: "Short identifier for cross-referencing between tasks"
        examples: ["task-1", "setup-db", "add-api-route"]
      description: string
      files:
        type: array
        desc: "Files this task will create or modify"
      dependencies:
        type: array
        desc: "IDs of tasks that must complete before this one"
      complexity: enum(low, medium, high)
  estimated_steps: number
  summary: string

markers:
  has_complex_tasks: "tasks[complexity == high].length > 0"
```

---

## Validation

When the engine parses structured output, it validates:

1. **JSON syntax** — the block between delimiters must be valid JSON
2. **Required fields** — all non-optional fields must be present
3. **Types** — values must match declared types (string, number, boolean)
4. **Enums** — values must be one of the declared options

Validation failures are treated as agent errors — the step fails, and retries
apply if configured. The error message includes which field failed validation
and why, so the retry prompt can be specific.

### Lenient parsing

The engine applies light normalization before strict validation:

- Strips markdown code fences if the agent wraps the JSON in ` ```json `
- Trims whitespace
- Accepts trailing commas (common LLM output artifact)

---

## Engine integration

### Workflow execution

Structured output integrates with the existing execution engine at these
points:

1. **Schema loading** — resolve `output` reference to a `.yaml` file, parse
   the schema definition
2. **Prompt building** (`build_agent_prompt`) — if the call has an `output`
   schema, generate schema-specific output instructions instead of the generic
   `CONDUCTOR_OUTPUT` instructions
3. **Output parsing** — parse the JSON between delimiters, validate against
   schema, extract derived markers and context
4. **Step record** — store the full structured output in
   `workflow_run_steps.structured_output` (new column, JSON text)
5. **Marker derivation** — compute markers from schema rules (or defaults) for
   `if`/`while` compatibility
6. **Context extraction** — use `summary` field (if present) as
   `{{prior_context}}`

### PR comment formatting

For reviewer agents with structured output, the PR comment aggregator can
switch from concatenating free text to rendering structured findings:

```markdown
## Security Review

| Severity | File | Line | Finding |
|----------|------|------|---------|
| high | src/auth.rs | 42 | SQL injection risk in query builder |
| medium | src/api.rs | 118 | Missing rate limit on /admin endpoint |

**Result:** 2 findings (1 high, 1 medium) — not approved
```

This formatting is engine-controlled, not agent-controlled, ensuring
consistency across all reviewers.

### Data availability for downstream steps

The full structured output from a prior step is available to downstream agents
via `{{prior_output}}` — the raw JSON object. This allows an implementation
agent to receive a planning agent's structured task list directly, rather than
parsing a summary string.

---

## Considered alternatives

### Schema embedded in agent frontmatter

The initial design put `output_schema` in the agent `.md` file's YAML
frontmatter. This makes each agent file self-contained.

**Why not chosen:** The schema describes how the workflow consumes output, not
a property of the agent. As the number of reviewers grows, every new reviewer
would need the same schema duplicated in its frontmatter. A change to the
findings format requires editing every reviewer file. Separating schemas into
standalone files and referencing them from the workflow eliminates this
duplication and keeps agent files focused on their prompt and role.

### Inline field descriptions

An early design used a pipe syntax for inline descriptions:

```yaml
category: string | "OWASP category or general area"
```

**Why not chosen:** Doesn't scale. Adding `examples`, `default`, or future
per-field metadata would require increasingly awkward inline syntax. The object
form (`type` + `desc` + `examples`) is slightly more verbose but extensible
without grammar changes.

### Full JSON Schema

Using standard JSON Schema for validation. Provides comprehensive features
including `oneOf`/`anyOf`, regex patterns, and numeric ranges.

**Why not chosen:** JSON Schema is verbose and unfamiliar to most workflow
authors. The simplified type system covers the practical cases (flat objects,
arrays of objects, enums) without the learning curve. Can be revisited if the
simplified system proves insufficient.

### Response format API parameter

The Claude API supports a `response_format` parameter that constrains output
to a JSON schema at the model level, guaranteeing valid structure.

**Why not chosen for now:** Conductor invokes `claude -p` via CLI, which does
not expose `response_format`. The prompt injection approach works reliably in
practice. If conductor moves to direct API calls (v2 daemon), `response_format`
would be a natural optimization — the prompt instructions would remain as
guidance, and the API constraint would guarantee compliance.

---

## Grammar changes

The `call` grammar extends to accept an `output` option:

```
call := "call" (IDENT | STRING) ("{" kv* "}")?
```

Where `kv` now includes `output = IDENT | STRING` alongside `retries` and
`on_fail`.

The `parallel` grammar extends similarly:

```
parallel := "parallel" "{" kv* call* "}"
```

Where the block-level `kv` set gains `output`.

---

## DB changes

```sql
ALTER TABLE workflow_run_steps ADD COLUMN structured_output TEXT;  -- full JSON
```

The existing `markers_out` and `context_out` columns continue to be populated
(from derived markers and the `summary` field respectively), maintaining
backward compatibility for queries and UI that read those columns.

---

## Future extensions

### Schema composition

Allow schemas to include other schemas:

```yaml
# .conductor/schemas/review-with-suggestions.yaml
include: review-findings
fields:
  suggested_changes:
    type: array
    items:
      file: string
      diff: string
```

Deferred until schema reuse patterns emerge.

### Typed context threading

Instead of `{{prior_context}}` being a string, allow downstream agents to
receive typed fields from the prior step's structured output:

```
The plan contains {{prior_output.tasks.length}} tasks.
The highest complexity task is: {{prior_output.tasks | max_by(complexity)}}
```

This requires a template expression language, which adds complexity. Deferred
until the simple `{{prior_output}}` JSON blob proves insufficient.
