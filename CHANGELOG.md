# Changelog

## [Unreleased]

### Added

- Claude CLI `--bare` mode support for agent runs. Conductor now launches the
  plan-generation call with `--bare` unconditionally (it's a one-shot JSON
  prompt with no tool use or project-context needs). For the main agent runner,
  bare mode is opt-in via a new `general.claude_bare_mode` config flag:

  ```toml
  [general]
  claude_bare_mode = true
  ```

  When enabled, agent runs skip Claude Code's auto-discovered startup
  (CLAUDE.md, agents, skills catalog, plugin sync, auto-memory, hooks),
  saving roughly 25-40k tokens per run. Conductor's own session-context
  injection is unaffected. Requires `ANTHROPIC_API_KEY` or `apiKeyHelper`
  auth — keychain OAuth is not read in bare mode. Defaults to `false`.

### Deprecated

- `[notifications.workflows]` — Use `[[notify.hooks]]` with `on` patterns instead.
  A deprecation warning is now emitted at startup when this section is present in `config.toml`.
  The struct will be removed in the next release.

#### Migration

**Before:**
```toml
[notifications.workflows]
on_failure = true
on_success = false
on_gate_human = true
on_gate_ci = false
on_gate_pr_review = true
```

**After:**
```toml
[[notify.hooks]]
on = "workflow_run.failed"
run = "notify-send 'Conductor' 'Workflow failed'"

[[notify.hooks]]
on = "gate.waiting"
url = "https://hooks.slack.com/services/..."
```

| Old flag | Equivalent hook `on` value |
|---|---|
| `on_failure = true` | `"workflow_run.failed"` |
| `on_success = true` | `"workflow_run.completed"` |
| `on_gate_human = true` | `"gate.waiting"` |
| `on_gate_ci = true` | `"gate.waiting"` |
| `on_gate_pr_review = true` | `"gate.waiting"` |
| Multiple flags | `"workflow_run.failed,gate.waiting"` (comma-separated OR) |

> **Note:** `on_gate_ci`, `on_gate_human`, and `on_gate_pr_review` all map to the same
> `gate.waiting` event — per-gate-type discrimination is not yet supported at the hook level.
