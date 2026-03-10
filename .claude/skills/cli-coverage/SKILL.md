---
name: cli-coverage
description: Audit all CLI commands and flag which ones lack TUI or Web coverage.
---

# cli-coverage

Produce a coverage matrix of every CLI subcommand vs. TUI and Web UI.

## Steps

### 1. Collect CLI commands

Read `conductor-cli/src/main.rs` and enumerate every subcommand (top-level and nested). Group them by command group (repo, worktree, tickets, agent, workflow, merge-queue, etc.).

### 2. Check TUI coverage

Search `conductor-tui/src/` for references to each command's underlying functionality:
- Look for the action name (e.g., `Push`, `CreatePr`, `LaunchAgent`)
- Look for calls to the relevant manager method

Mark ✅ if the TUI exposes the functionality, ❌ if not.

### 3. Check Web coverage

Search `conductor-web/src/` for HTTP route handlers corresponding to each command:
- Match by manager method name or HTTP path pattern
- Check `conductor-web/src/routes/` for relevant route files

Mark ✅ if a web endpoint exists, ❌ if not.

### 4. Output the matrix

Print a table grouped by command group:

```
## <Group Name>
| Command | TUI | Web |
|---|---|---|
| `group sub` | ✅/❌ | ✅/❌ |
```

After all groups, print a summary section:

```
## CLI-Only Commands (neither TUI nor Web)
- `command sub` — one-line description of what it does
```

And a quick stats line:
```
X / Y commands covered by both TUI and Web.
```

### 5. Flag for action

For any CLI-only commands, note whether they appear to be:
- **Dead code** — no workflows, agents, or tests reference them
- **Automation-only** — used by CI/scripts but not interactive UIs (acceptable)
- **TUI gap** — functionality that users would reasonably want in the TUI

Suggest which CLI-only commands are candidates for removal vs. intentional design.
