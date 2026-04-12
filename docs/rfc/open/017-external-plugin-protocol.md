# RFC 017: External Plugin Protocol

**Status:** Draft
**Date:** 2026-04-12
**Author:** Devin

---

## Problem

Conductor's extensibility points today are closed: ticket sources and lifecycle integrations require first-party Rust code changes to add. Users who want to sync from Linear, Shortcut, Azure DevOps, or any bespoke internal tracker must wait for a Conductor release. The same is true for any custom action a user wants to trigger on a conductor lifecycle event (worktree created, ticket synced, etc.) beyond the notification use case RFC 011 already covers.

### Costs we are paying today

**1. Hardcoded ticket sources.** `IssueSourceManager` supports GitHub and Jira. Any new source — Linear, Shortcut, Notion, an internal JIRA-alike — requires a new crate dependency, new config structs, new sync logic, and a PR to conductor. The long tail of issue trackers is unbounded; conductor cannot own all of them.

**2. No lifecycle extensibility.** RFC 011 (notification hooks) fires on workflow and agent terminal states. It does not cover lower-level conductor events: worktree created/deleted, ticket synced, repo registered. Users cannot trigger custom actions (update an external system, write a log entry, kick off a side process) on these events.

Both problems have the same root cause: conductor is in the business of implementing integrations rather than defining a stable contract that third parties implement.

---

## Proposed Design

### Core idea

Define a **subprocess protocol** that third-party ticket sources must implement, and extend the RFC 011 hook system with additional lifecycle event types. Conductor owns the contracts; users and the community own the implementations.

```
Ticket source plugin:
conductor ──► spawn(plugin, ["list"], env) ──► read stdout (JSON lines) ──► upsert tickets

Lifecycle hooks (extends RFC 011):
conductor ──► lifecycle event fires ──► HookRunner::fire(&event) ──► run/url hook
```

---

## Part 1: External Ticket Sources

### Invocation model

One-shot subprocess per command. Conductor spawns the plugin binary, passes the command as the first argument, reads newline-delimited JSON from stdout, and waits for the process to exit. No persistent daemon; no long-lived connection.

This fits naturally with `std::process::Command`, which is already how all git ops and `gh` CLI work.

### Command contract

Three commands. All output newline-delimited JSON to stdout. One JSON object per line.

```bash
my-plugin list          # list all open tickets → one JSON object per line
my-plugin get <id>      # get a single ticket by source_id → one JSON object
my-plugin sync          # optional: signal that a full refresh is requested
                        #           may output the same shape as list, or nothing
```

`sync` is advisory — conductor calls it when the user explicitly triggers a sync. A plugin that does not distinguish between `list` and `sync` may implement them identically.

**Ticket output shape:**

```json
{
  "source_id": "abc-123",
  "title": "Fix login bug",
  "body": "Users on iOS 17 cannot log in after the OAuth change.",
  "status": "open",
  "url": "https://linear.app/acme/issue/ENG-123"
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `source_id` | string | yes | Stable identifier within this source. Used as the upsert key. |
| `title` | string | yes | Ticket title / summary. |
| `body` | string | no | Full description. Empty string if not available. |
| `status` | string | yes | `"open"` or `"closed"`. Conductor filters closed tickets from active views. |
| `url` | string | no | Deep link to the ticket in the external system. |

Additional fields are allowed and silently ignored — forward compatibility for future protocol versions.

### Exit codes

- `0` — success. Conductor processes stdout.
- Non-zero — error. Conductor reads stderr and surfaces it as a user-facing sync error. Existing tickets in the DB are not wiped.

### Context via environment

Conductor injects context as environment variables. Plugins read these instead of parsing complex arguments.

```
CONDUCTOR_REPO_SLUG=conductor-ai
CONDUCTOR_REPO_PATH=/Users/devin/Personal/conductor-ai
CONDUCTOR_PLUGIN_CONFIG={"workspace":"acme","project":"ENG"}
CONDUCTOR_PLUGIN_NAME=linear-conductor-plugin
```

`CONDUCTOR_PLUGIN_CONFIG` is the raw JSON serialization of the `config` table from `config.toml` (see Config schema below). Plugins are responsible for parsing and validating their own config.

### Config schema (`config.toml`)

```toml
[[repos.conductor-ai.issue_sources]]
type = "external"
plugin = "linear-conductor-plugin"   # binary name (PATH lookup) or absolute path
config = { workspace = "acme", project = "ENG" }

[[repos.conductor-ai.issue_sources]]
type = "external"
plugin = "/usr/local/bin/my-custom-source"
config = { api_url = "https://internal.corp/tracker", token_env = "CORP_API_TOKEN" }
```

Multiple external sources per repo are allowed. Each runs independently; results are merged.

```rust
pub struct ExternalIssueSourceConfig {
    /// Binary name (resolved via PATH) or absolute path.
    pub plugin: String,
    /// Arbitrary key-value config passed to the plugin as CONDUCTOR_PLUGIN_CONFIG JSON.
    pub config: Option<toml::Value>,
    /// Per-sync timeout in milliseconds (default: 30_000).
    pub timeout_ms: Option<u64>,
}
```

### DB integration

External sources slot into the existing `repo_issue_sources` schema using `source_type = "external:<plugin_name>"`. The existing `ON CONFLICT DO UPDATE` upsert on `(repo_id, source_type, source_id)` handles idempotency with no schema changes.

Example: a plugin named `linear-conductor-plugin` producing ticket `ENG-123` yields:

```
source_type = "external:linear-conductor-plugin"
source_id   = "ENG-123"
```

A single nullable `plugin_path` column is added to `repo_issue_sources` to persist the resolved plugin path for a given source row:

```sql
-- Migration 065
ALTER TABLE repo_issue_sources ADD COLUMN plugin_path TEXT;
```

### Error handling

| Scenario | Behavior |
|---|---|
| Non-zero exit | Surface stderr as sync error. Do not wipe existing tickets. |
| Timeout (default 30s) | Kill process. Surface as sync error. |
| Malformed JSON line | Skip the line, log a warning, continue. Partial sync beats full failure. |
| Plugin binary not found | Surface as config error at sync time, not at startup. |
| Empty stdout | Success with zero tickets upserted. Not an error. |

### Example plugin (Python)

```python
#!/usr/bin/env python3
"""Linear ticket source for conductor."""
import sys, json, os
import urllib.request

config = json.loads(os.environ.get("CONDUCTOR_PLUGIN_CONFIG", "{}"))
api_key = os.environ[config.get("token_env", "LINEAR_API_KEY")]

QUERY = """
{ issues(filter: { state: { type: { nin: ["completed", "cancelled"] } } }) {
    nodes { id title description state { name } url }
} }
"""

def fetch_issues():
    req = urllib.request.Request(
        "https://api.linear.app/graphql",
        data=json.dumps({"query": QUERY}).encode(),
        headers={"Authorization": api_key, "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())["data"]["issues"]["nodes"]

cmd = sys.argv[1] if len(sys.argv) > 1 else "list"

if cmd in ("list", "sync"):
    for issue in fetch_issues():
        print(json.dumps({
            "source_id": issue["id"],
            "title": issue["title"],
            "body": issue.get("description") or "",
            "status": "open",
            "url": issue.get("url") or "",
        }))
elif cmd == "get":
    source_id = sys.argv[2]
    for issue in fetch_issues():
        if issue["id"] == source_id:
            print(json.dumps({
                "source_id": issue["id"],
                "title": issue["title"],
                "body": issue.get("description") or "",
                "status": "open",
                "url": issue.get("url") or "",
            }))
            break
```

Testable in isolation: `CONDUCTOR_PLUGIN_CONFIG='{}' LINEAR_API_KEY=lin_... ./linear-plugin list`

---

## Part 2: Lifecycle Hooks

RFC 011 (implemented) fires notification hooks on workflow and agent terminal states. This RFC extends the same `[[notify.hooks]]` system with additional event types covering lower-level conductor lifecycle events.

No new hook infrastructure is needed — `HookRunner::fire()` and the env var payload model from RFC 011 are reused unchanged.

### New event types

| Event | When |
|---|---|
| `worktree.created` | A new worktree is created for a repo |
| `worktree.deleted` | A worktree is deleted |
| `ticket.synced` | A ticket sync cycle completes (any source) |
| `repo.registered` | A repo is registered with conductor |
| `repo.unregistered` | A repo is removed from conductor |

These extend the RFC 011 taxonomy. Glob matching works the same way: `worktree.*` matches both `worktree.created` and `worktree.deleted`.

### Payload fields

**`worktree.created` / `worktree.deleted`:**

```
CONDUCTOR_EVENT=worktree.created
CONDUCTOR_REPO_SLUG=conductor-ai
CONDUCTOR_WORKTREE_SLUG=feat-123-new-thing
CONDUCTOR_WORKTREE_BRANCH=feat/123-new-thing
CONDUCTOR_WORKTREE_PATH=/Users/devin/.conductor/workspaces/conductor-ai/feat-123-new-thing
CONDUCTOR_TIMESTAMP=2026-04-12T09:00:00Z
```

**`ticket.synced`:**

```
CONDUCTOR_EVENT=ticket.synced
CONDUCTOR_REPO_SLUG=conductor-ai
CONDUCTOR_SOURCE_TYPE=github            # or "external:linear-conductor-plugin"
CONDUCTOR_TICKETS_UPSERTED=14
CONDUCTOR_TICKETS_CLOSED=2
CONDUCTOR_TIMESTAMP=2026-04-12T09:00:00Z
```

**`repo.registered` / `repo.unregistered`:**

```
CONDUCTOR_EVENT=repo.registered
CONDUCTOR_REPO_SLUG=conductor-ai
CONDUCTOR_REPO_PATH=/Users/devin/Personal/conductor-ai
CONDUCTOR_TIMESTAMP=2026-04-12T09:00:00Z
```

### Example use cases

```toml
# Write a log entry on every worktree creation
[[notify.hooks]]
on = "worktree.created"
run = "echo \"$(date) worktree $CONDUCTOR_WORKTREE_SLUG created\" >> ~/conductor-audit.log"

# Notify a team channel when a ticket sync finds new work
[[notify.hooks]]
on = "ticket.synced"
run = "~/.conductor/hooks/ticket-sync-notify.sh"

# Update an external dashboard when a repo is registered
[[notify.hooks]]
on = "repo.registered"
url = "https://internal.corp/conductor-webhook"
headers = { "Authorization" = "$CORP_WEBHOOK_TOKEN" }
```

---

## Decisions Made

1. **One-shot subprocess, not daemon.** Ticket sync is infrequent; startup overhead is acceptable. Daemon plugins add lifecycle complexity (health checks, restart logic) that isn't warranted for a polling use case.

2. **Newline-delimited JSON stdout.** Language-agnostic. Streaming-compatible (conductor can begin upsetting as lines arrive, though it waits for exit before committing). Easier to debug than a binary protocol.

3. **Context via environment variables.** Consistent with RFC 011 hook payload pattern. No argument parsing complexity in plugins.

4. **`source_type = "external:<plugin_name>"` namespacing.** Avoids collision between multiple external plugins and with built-in sources. Sortable in the UI alongside `github` and `jira`.

5. **Partial sync on malformed output.** A plugin that emits one bad line should not discard good data. Skip and warn beats fail-closed.

6. **Lifecycle hooks extend RFC 011, not a new system.** `HookRunner` already exists and is proven. Reusing it avoids a second hook dispatch path and keeps the `[[notify.hooks]]` config familiar to users who already configure notification hooks.

7. **No plugin registry in v1.** Plugins are resolved via PATH or absolute path. Discovery is the user's responsibility. A registry is a v2 concern.

8. **`plugin_path` stored in DB.** Enables audit trails and ensures the exact binary used for a sync is recoverable, even if `config.toml` is changed later.

---

## Open Questions

1. **Bidirectional plugins.** Can a plugin push a ticket *back* to conductor (e.g. a webhook receiver that inserts tickets as they arrive rather than on a poll cycle)? This would require an inbound HTTP API surface — out of scope for v1 but worth tracking.

2. **Plugin auth and sandboxing.** Plugins run with the same privileges as conductor. Users who install community plugins accept this. Should conductor warn when invoking a plugin from outside a trusted path (e.g. outside `~/.conductor/plugins/`)? Probably a v2 concern.

3. **Plugin versioning.** The protocol has no version field today. If the output schema changes, old plugins break silently. Should `list` output include a `protocol_version` field for forward compatibility?

4. **`get` command necessity.** Conductor currently calls `list` for sync and has no single-ticket fetch path. `get` is defined in the protocol for future use (e.g. fetching a ticket before launching an agent). Is it worth requiring plugins to implement it in v1, or should it be optional?

5. **Multiple external sources per repo, same plugin.** A user might want two Linear projects from the same binary with different `config` values. The `source_type = "external:<plugin_name>"` key would collide. Should the key include a user-defined label, e.g. `"external:linear:eng"` and `"external:linear:design"`?

---

## Implementation Order

**PR 1 — External ticket sources:**
1. DB migration 065: add `plugin_path` column to `repo_issue_sources`
2. `ExternalIssueSourceConfig` struct and TOML parsing
3. `ExternalTicketSource::sync()`: spawn plugin, drain newline-delimited JSON, upsert via existing `TicketSyncer`
4. Wire into `IssueSourceManager`: detect `type = "external"`, dispatch to `ExternalTicketSource`
5. CLI: surface external source errors in `conductor ticket sync` output
6. TUI: surface external source errors in the sync status modal

**PR 2 — Lifecycle hooks:**
7. Add `worktree.created`, `worktree.deleted` events: fire from `WorktreeManager::create()` and `WorktreeManager::delete()`
8. Add `ticket.synced` event: fire from `TicketSyncer::sync()` with upsert/close counts
9. Add `repo.registered`, `repo.unregistered` events: fire from `RepoManager::register()` and `RepoManager::unregister()`
10. Extend `NotificationEvent` enum and `to_payload()` with new variants
11. Add `docs/examples/plugins/` with the Linear Python example and a minimal shell stub

PRs are independent and can land in either order.

---

## Out of Scope

- Plugin registry or package manager
- Bidirectional plugins (inbound webhook receivers)
- Web UI for plugin management or configuration
- Plugin sandboxing or WASM isolation
- Windows support (same assumption as RFC 016)
- Jira external-source migration (Jira stays first-party; this RFC adds a path for everything else)
