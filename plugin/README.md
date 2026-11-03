# conductor — Claude Code plugin

Reports interactive Claude Code sessions to conductor (RFC 021) by calling
`conductor session hook` on `SessionStart`, `UserPromptSubmit`, `PermissionRequest`,
`PostToolUse`, `PostToolUseFailure`, `Stop` and `SessionEnd`. Sessions started in a
conductor worktree, whether by conductor or by hand, then show up in the TUI and web UI.

## Install

```bash
claude plugin marketplace add devinrosen/conductor-ai
claude plugin install conductor@conductor-ai
```

If you installed the LivelyVideo copy, remove it first (both plugins are named `conductor`):

```bash
claude plugin uninstall conductor@lively-video
```

## Requirements

- `conductor` >= 0.17.0 on `PATH`. With an older binary the hooks are silent no-ops.
- Unix (macOS/Linux): the wrapper is POSIX `sh`.

## What is tracked

Only attended (interactive) sessions whose `cwd` is inside an **active** conductor worktree.
Everything else is a silent no-op: headless `claude -p` sessions (including conductor's own
workflow agents), a repo's main checkout, unregistered projects, merged or abandoned worktrees.

The wrapper always exits 0 with no output. Set `CONDUCTOR_SESSION_HOOK_DEBUG=1` to keep stderr.

## Scope

Hooks only. The MCP server is still registered by `conductor setup install`, and the
workflow skills stay project-scoped.

The plugin version tracks the conductor release; the release commit bumps both.

## Caveat for launchers

Do not also pass `--settings` with these same hooks to `claude` when the plugin is installed,
or every event fires twice (#3158).
