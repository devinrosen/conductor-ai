# Ideas Parking Lot

Half-formed ideas that aren't ready for a ticket or RFC. Promote to a GitHub issue when the design questions are resolved.

---

## Tool version / update warnings

Surface a warning when required external tools (`gh`, `git`, `tmux`, package managers) are missing or outdated.

**Open questions:**
- When to check? Startup adds latency (subprocess calls). Background check with a dismissable footer warning may be better.
- Proactive (define minimum versions, check on startup) vs reactive (catch version-specific failures at the call site with a helpful hint like "consider upgrading `gh`")?
- Proactive version pinning for all tools could become maintenance whack-a-mole — reactive with better error messages may be the right default.
- How to surface it? Dismissable footer message, not a hard block.

---

## CI status in TUI

Show PR check status (passing/failing/pending) without leaving the TUI.

**Open questions:**
- Where first? Options in priority order: (1) dashboard worktree list as a small indicator, (2) worktree detail view, (3) persistent workflow column from #662.
- Data source: `gh pr view --json statusCheckRollup` for list/summary view; `gh pr checks` for per-check breakdown in detail view.
- Fetch strategy: on-demand (manual refresh key) first, background polling later. CI status is volatile (checks run for minutes) so auto-refresh adds meaningful load.
- Only relevant for worktrees with an open PR.

---
