# RFC 018: Feature Epic Orchestration

**Status:** Closed — superseded by [DIRECTION.md](../../DIRECTION.md); replacement plan in [IDEAS-feature-table-replacement.md](../../IDEAS-feature-table-replacement.md)
**Date:** 2026-04-12
**Closed:** 2026-04-16
**Author:** Devin

---

> **Superseded 2026-04-16.** This RFC was accepted in April 2026 and partial schema changes landed (migration 070 — `source_type`, `source_id`, `tickets_total`, `tickets_merged`; expanded status CHECK). The remaining elevation — `conductor feature create`, fan-out orchestration, the feature status machine — is superseded by the compact-to-core direction in [DIRECTION.md](../../DIRECTION.md). The `features` table and `FeatureManager` are candidates for removal in favor of `foreach over = tickets` workflows; see [IDEAS-feature-table-replacement.md](../../IDEAS-feature-table-replacement.md). The landed schema columns stay in place to avoid a down-migration; they will be removed when the table itself is removed.

---

## Problem

Conductor's current workflow is optimized for a single ticket → single worktree → PR to `main`. This works well for isolated changes but breaks down for multi-ticket epics where:

- Multiple related tickets need to land together before they're meaningful to review
- QA and product teams need to evaluate a cohesive feature, not individual PRs
- The merge target isn't `main` — it's an integration branch for the epic

The `features` table exists in the schema and is referenced in 5 workflows via `{{feature_base_branch}}`, but the concept is underdeveloped. Features are auto-created as a side effect of worktree creation, there is no way to create or manage them explicitly, and their lifecycle is invisible — leading to phantom entries in the branch picker when cleanup fails (see: `feat/1737` bug, fixed via raw SQL).

---

## Proposed Design

Elevate `features` to the primary orchestration unit for multi-ticket work. A feature maps to a GitHub milestone or Jira epic, owns a long-lived integration branch, and acts as a queue of tickets that agents work through — each in its own ephemeral worktree targeting the feature branch. The feature branch merges to `main` only after QA and product approval.

### Lifecycle

```
GitHub Milestone / Jira Epic
        │
        ▼ (conductor feature create)
  Feature record ──── status: in_progress
        │
        ▼ (ticket sync from milestone/epic)
  feature_tickets queue
     ticket-101 ──► worktree ──► agent ──► PR to feature branch ──► merged
     ticket-102 ──► worktree ──► agent ──► PR to feature branch ──► merged
     ticket-103 ──► worktree ──► agent ──► PR to feature branch ──► merged
        │
        ▼ (all tickets merged)
  Feature status: ready_for_review
        │
        ▼ (QA + product evaluate feature branch)
  Feature status: approved
        │
        ▼ (feature branch → main)
  Feature status: merged
```

### Status Machine

| Status | Meaning |
|---|---|
| `in_progress` | Tickets are being worked |
| `ready_for_review` | All tickets merged; handed off to QA/product |
| `approved` | Reviewed and approved; ready to merge to main |
| `merged` | Feature branch merged to main |
| `closed` | Abandoned or cancelled |

### Key Behaviors

**Explicit creation only.** Features are no longer auto-created when a worktree is created. A feature must be explicitly created from a milestone/epic, establishing the feature branch and ticket queue. Worktrees created from a feature's ticket queue inherit `base_branch = feature.branch`.

**Milestone/epic sync.** `conductor feature create --milestone <id>` (or `--epic <id>` for Jira) fetches all open issues from the source and populates `feature_tickets`. Subsequent `conductor feature sync` calls pull in newly added issues and close tickets that were removed from the milestone.

**Fan-out orchestration.** `conductor feature run <name>` spawns a worktree and agent for each open ticket in the feature queue, up to a configurable parallelism limit. Agents run concurrently, each targeting the feature branch.

**Progress tracking.** Features expose `tickets_total`, `tickets_merged`, and `tickets_open` counts derived from `feature_tickets` join state. This surfaces as a progress indicator in the TUI and web.

**Ready-for-review transition.** When the last ticket's PR merges into the feature branch, the feature automatically transitions to `ready_for_review`. A notification (via RFC-013 push, if configured) is sent to the configured QA channel or recipient.

**Dangling feature reaper.** On startup and periodic tick (matching the orphan agent reaper pattern), features with `status = 'in_progress'` and `worktree_count = 0` and no open PRs are flagged as `dangling`. Dangling features surface a warning in the TUI/web and can be explicitly closed or re-activated. This replaces the raw-SQL workaround for the `feat/1737` class of bugs.

---

## Schema Changes

### `features` table (existing, modified)

Add columns:
- `source_type TEXT` — `github_milestone`, `jira_epic`, or `manual` (free-form; no CHECK constraint so new source types never require a migration)
- `source_id TEXT` — globally-namespaced identifier (nullable for manual); see format below
- `status TEXT NOT NULL DEFAULT 'in_progress'` — replaces implicit open/closed
- `tickets_total INTEGER NOT NULL DEFAULT 0` — denormalized count, updated on sync
- `tickets_merged INTEGER NOT NULL DEFAULT 0` — updated when worktree PR merges

**`source_id` format** (globally namespaced to not foreclose multi-repo support):

| `source_type` | `source_id` format | Example |
|---|---|---|
| `github_milestone` | `github.com/{owner}/{repo}/milestones/{number}` | `github.com/acme/api/milestones/42` |
| `jira_epic` | `{jira_base_url}/browse/{epic_key}` | `acme.atlassian.net/browse/PLAT-100` |
| `manual` | `NULL` | — |

`repo_id` stays required in v1 (features are still per-repo). When multi-repo support is added, `repo_id` becomes nullable and a `feature_repos` join table is introduced — no `source_id` migration needed because the format is already globally unique.

Remove implicit behavior:
- Drop trigger/code that auto-creates features on worktree creation

### `feature_tickets` table (existing, unchanged)

No schema change. The join table already supports the ticket queue pattern. Usage becomes more intentional — populated via milestone/epic sync rather than manual linking.

---

## CLI

```
conductor feature create <repo> <name> --branch <branch> [--base <base>]
conductor feature create <repo> <name> --milestone <id>
conductor feature create <repo> <name> --epic <id>

conductor feature list <repo>
conductor feature sync <repo> <name>        # re-pull tickets from source
conductor feature run <repo> <name> [--parallel <n>]
conductor feature review <repo> <name>      # transition to ready_for_review
conductor feature approve <repo> <name>     # transition to approved
conductor feature close <repo> <name>
```

---

## Config

Two new fields in `[general]` (`config.toml`):

```toml
[general]
# Max concurrent agent runs when using `conductor feature run`.
# Override per-invocation with --parallel <n>.
max_feature_parallelism = 3

# Automatically transition a feature to ready_for_review when its last
# worktree is marked merged. Set to false to require an explicit
# `conductor feature review` call instead.
auto_ready_for_review = true
```

---

## Fan-out Behavior

### Parallelism

`conductor feature run` reads `general.max_feature_parallelism` (default **3**) and spawns at most that many agents concurrently. Remaining tickets are queued; each time a running agent finishes, the next ticket in the queue is dispatched. The `--parallel <n>` flag overrides the config value for that invocation.

3 is the right default — it balances review bandwidth (3 concurrent PRs against a feature branch is already a heavy review load), GitHub API pressure from simultaneous polling, and disk consumption from concurrent full-checkout worktrees.

### Partial fan-out (skip detection)

`conductor feature run` skips a ticket if it already has a worktree in `active` or `merged` status for the same repo:

```sql
SELECT t.id FROM tickets t
JOIN feature_tickets ft ON ft.ticket_id = t.id
WHERE ft.feature_id = ?1
  AND NOT EXISTS (
    SELECT 1 FROM worktrees w
    WHERE w.ticket_id = t.id
      AND w.repo_id = ?2
      AND w.status IN ('active', 'merged')
  )
```

- `active` → in-flight, skip
- `merged` → done, skip
- `abandoned` → retry-eligible, include

This is a pure DB query with no `gh` API call at fan-out time. The edge case of an abandoned worktree with an open PR is not handled in v1 — the agent will discover the existing PR and either update it or fail with a clear message.

---

## Ready-for-Review Automation

The `ready_for_review` transition fires **automatically** when `cleanup_merged_worktrees` marks the last active worktree for a feature as `merged` (i.e., when its active worktree count hits zero). A notification is dispatched via RFC-013 push if configured.

When `general.auto_ready_for_review = false`, the transition requires an explicit `conductor feature review` call. The automatic path only changes a status field — it does not merge the feature branch to `main` — so a false-positive `ready_for_review` is harmless.

---

## TUI & Web Changes

**TUI:** Add a Features view (alongside Repos, Worktrees, Tickets) showing feature name, status, progress bar (`tickets_merged / tickets_total`), and staleness. Key bindings: `r` to run fan-out, `v` to mark ready-for-review, `a` to approve, `x` to close.

**Web:** Add a Features page with the same data. The `ready_for_review` transition surfaces a "Hand off to QA" button. Dangling features show an inline warning with a "Close" action so users are never forced into raw SQL.

---

## Workflow Engine Changes

**`feature_base_branch` injection stays.** The existing `inject_feature_variables()` behavior is unchanged — `feature_id`, `feature_name`, `feature_branch`, and `feature_base_branch` continue to be injected when a workflow run has a `feature_id`. The 5 existing `.wf` files that rely on `{{feature_base_branch}}` require no changes.

**Fan-out step (future).** The `conductor feature run` fan-out could eventually be expressed as a workflow step (related to RFC-010 for-each). That is out of scope for this RFC.

---

## Deferred

- **Multi-repo features.** The `source_id` format is globally namespaced so no migration is needed when cross-repo support is added. `repo_id` becoming nullable and a `feature_repos` join table are the two schema changes required at that time.
- **Jira support.** `source_type = 'jira_epic'` is reserved in the schema from day one. The sync implementation (calling `sync_jira_issues_acli()` with a JQL like `"Epic Link" = {key}`) is a follow-on. The existing `jira_acli.rs` module and `JiraConfig` struct are the integration points.

---

## What This Does NOT Change

- The ticket → worktree → agent → PR flow for standalone work (no feature) is unchanged
- `WorktreeManager` auto-creation behavior for non-feature worktrees is unchanged
- Existing `.wf` files require no edits
- The `features` table is not removed — it is promoted
