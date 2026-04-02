# RFC 006: Workflow-Driven Ticket Sources

**Status:** Closed — superseded by [RFC 009](../open/009-ticket-dependency-graph.md)
**Date:** 2026-03-16
**Closed:** 2026-04-01
**Author:** Devin
**Closes:** [#146](https://github.com/devinrosen/conductor-ai/issues/146)

---

## Problem

Conductor currently supports GitHub Issues and Jira as hardcoded ticket sources. Teams using other systems (Linear, Shortcut, Azure DevOps, Vantage, in-house trackers) have no integration path without modifying conductor's core. Issue #146 proposed a CLI plugin system to solve this. That design is still valid but has been superseded by a cleaner approach now that workflows are the established extension mechanism in conductor.

---

## Proposed Solution

Replace the hardcoded ticket source dispatch with workflow-driven ticket operations. Each repo configures up to three optional workflows:

- **sync** — pulls tickets from the external source and upserts them into conductor's DB
- **create** — creates a ticket in the external system, returns `{id, url}`
- **update** — updates an existing ticket (status, title, labels, etc.)

Conductor ships built-in workflows for GitHub and Jira (replacing the hardcoded implementations). Teams wire in any other source by writing their own `.wf` file and configuring it for their repo. Agent prompts and CLI commands route through a unified `conductor ticket <op>` interface regardless of the configured source.

---

## Design

### Ticket operations as workflows

Each operation is a standard conductor workflow. The `create` workflow receives inputs (`title`, `body`, `labels`) and emits structured output (`{id, url}`). The `sync` workflow calls `conductor ticket upsert` to write tickets into the DB. The `update` workflow receives a ticket ID and changed fields.

```yaml
# Example: .conductor/workflows/ticket-create.wf
name: create-ticket
inputs:
  - name: title
    type: string
  - name: body
    type: string
  - name: labels
    type: string

steps:
  - id: create
    type: script
    run: |
      gh issue create \
        --repo "$CONDUCTOR_REPO_SLUG" \
        --title "{{ inputs.title }}" \
        --body "{{ inputs.body }}" \
        --label "{{ inputs.labels }}"
```

### Per-repo `.conductor/config.toml`

Ticket workflow configuration lives in a per-repo `.conductor/config.toml` committed to the repository. This keeps ticket source config versioned with the code and visible to the whole team. Global `~/.conductor/config.toml` retains user-level settings (auth tokens, UI preferences).

```toml
# <repo-root>/.conductor/config.toml

[tickets]
sync_workflow   = ".conductor/workflows/ticket-sync.wf"
create_workflow = ".conductor/workflows/ticket-create.wf"
update_workflow = ".conductor/workflows/ticket-update.wf"
```

### `conductor ticket` CLI commands

Three new subcommands route through the configured workflow:

```
conductor ticket sync   --repo <slug>
conductor ticket create --repo <slug> --title <t> --body <b> [--labels <l>]
conductor ticket update --repo <slug> --id <id> [--status <s>] [--title <t>]
```

If no workflow is configured for an operation, conductor returns a clear error with instructions for how to configure one.

### Built-in GitHub and Jira workflows

The existing hardcoded `github.rs` and `jira_acli.rs` implementations are converted to `.wf` workflow files shipped with conductor. They serve as both the default implementation for current users and as reference examples for teams integrating other sources.

### `conductor ticket upsert` CLI command

The sync workflow needs a way to write tickets into conductor's DB. A new CLI command is required:

```
conductor ticket upsert \
  --repo <slug> \
  --source-id <id> \
  --source-type <type> \
  --title <t> \
  --body <b> \
  --status <s> \
  --url <url>
```

`TicketSyncer::upsert_tickets()` and `TicketInput` already exist in `conductor-core` — the CLI subcommand is a thin wrapper that is not yet exposed. This needs to be added.

### Listing

No list workflow. `conductor ticket list` remains a direct DB read. Sync workflows write to the DB; listing reads from it. The TUI ticket view is unchanged.

### Sync trigger

Sync is manually invoked (via `conductor ticket sync`) until the daemon (#9) is available for scheduled/webhook-triggered syncs.

---

## Migration Path for Existing GitHub/Jira Users

Existing repos with `source_type = "github"` or `"jira"` in `repo_issue_sources` need a migration path when the hardcoded implementations are removed. Options:

1. **Auto-provision on upgrade** — on first run after upgrade, conductor detects the existing source type and writes the built-in workflow config into the repo's `.conductor/config.toml`.
2. **Parallel track with deprecation warning** — keep the hardcoded path as a fallback, print a deprecation warning on use, remove in a later version.

Recommendation: option 2 first (lower risk), followed by option 1 once per-repo config is stable.

---

## Open Questions

1. **Per-repo config discovery** — How does conductor find `.conductor/config.toml`? Walk up from the current directory? Use the registered repo path in the DB? What happens when running from outside a worktree?

2. **Global vs. per-repo config precedence** — What wins when a setting exists in both `~/.conductor/config.toml` and `<repo>/.conductor/config.toml`? Per-repo should probably win for ticket settings.

3. **`.conductor/` in `.gitignore`** — Currently `.conductor/` may contain things that should not be committed. With per-repo config, some of it should be committed (`.wf` files, `config.toml`) and some should not (local state, caches). Need a clear convention or a separate committed directory name.

4. **Workflow output format** — How does `conductor ticket create` read structured output (`{id, url}`) from the workflow? This requires the workflow engine to support structured return values, not just exit codes. Needs design.

5. **Secrets in workflows** — Jira and other sources need API tokens. Workflows inherit env vars from the shell today. Is that sufficient, or does conductor need a secrets config layer?

---

## Dependencies

- **[PR #1135](https://github.com/devinrosen/conductor-ai/pull/1135) — Workflow bool inputs:** Must land first. String input support needs to be added on top of it before ticket workflows can receive `title`, `body`, etc.
- **Per-repo `.conductor/config.toml`:** A new architectural concept that likely warrants its own RFC or issue before being built as part of this. Affects more than just ticketing.
- **Structured workflow outputs:** `conductor ticket create` needs `{id, url}` back from the workflow. If the engine only supports exit codes today, this is a prerequisite.
- **[Issue #9](https://github.com/devinrosen/conductor-ai/issues/9) — Daemon:** Required for scheduled/webhook-triggered sync. Out of scope for this RFC; sync is manual until then.

---

## What This Closes

- **[#146](https://github.com/devinrosen/conductor-ai/issues/146)** — Plugin system for custom ticket sources. The workflow-driven approach covers the same use case with no new plugin infrastructure.

---

## Next Steps

- [ ] Add `conductor ticket upsert` CLI subcommand (core logic exists in `TicketSyncer::upsert_tickets()` — just needs CLI exposure)
- [ ] Open a focused issue for per-repo `.conductor/config.toml` (prerequisite)
- [ ] Design structured workflow output format (prerequisite)
- [ ] Extend workflow inputs to support string types (building on #1135)
- [ ] Convert `github.rs` sync/create to `.wf` files as reference implementations
- [ ] Implement `conductor ticket create/sync/update` routing commands
- [ ] Define migration path for existing GitHub/Jira `repo_issue_sources` rows
