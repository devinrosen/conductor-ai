# RFC 008: Feature Branch Coordination

**Status:** Implemented
**Date:** 2026-03-18
**Author:** Devin

---

## Problem

Conductor manages worktrees and PRs for individual tickets, but has no concept of grouping related work under a shared branch. When a feature or release spans multiple tickets (e.g., 5 issues for "notification improvements"), each worktree branches from `main` and each PR targets `main` independently.

In practice, teams want to:

1. Create a **feature branch** (e.g., `feat/notification-improvements`) that collects related work
2. Create **worktrees for individual tickets** that branch from and PR into the feature branch
3. Create a **rollup PR** from the feature branch into `main` when all sub-work is complete
4. Use the same mechanism for **release branches** that aggregate multiple features

### What Already Exists

Conductor already has the plumbing for non-main base branches:

- `worktree create --from <branch>` — branch from any base
- `worktrees.base_branch` column — stored per worktree
- `effective_base()` method — PR targeting uses the stored base branch
- `conductor worktree pr` — passes `--base` to `gh pr create`

The gap is **no first-class concept of a named branch as a coordination point** — no creation command, no grouping, no progress tracking, no rollup PR support.

---

## Proposed Design

### Core Concept

A **feature** is a named branch that worktrees (and other features) can target. Whether it's called a "feature branch" or "release branch" is a naming convention — the data model is the same.

Features are **purely local** to conductor. They do not sync with GitHub milestones, Jira epics, or any external grouping mechanism. This keeps the design agnostic to the ticketing system.

### Nesting

Features can target other features, enabling natural hierarchy:

```
main
└── release/2.0                          (feature targeting main)
    ├── feat/notification-improvements   (feature targeting release/2.0)
    │   ├── fix/1262-blocker-notifs      (worktree targeting feat/...)
    │   ├── fix/1263-notif-filtering     (worktree targeting feat/...)
    │   └── fix/1264-blocked-on          (worktree targeting feat/...)
    └── feat/multi-runtime-agents        (feature targeting release/2.0)
        ├── fix/add-runtime-trait         (worktree targeting feat/...)
        └── fix/add-gemini-runtime        (worktree targeting feat/...)
```

No special "release" concept is needed. A release branch is just a feature whose base is `main` and whose children are other features.

### Data Model

```sql
CREATE TABLE features (
    id TEXT PRIMARY KEY,              -- ULID
    repo_id TEXT NOT NULL REFERENCES repos(id),
    name TEXT NOT NULL,               -- e.g. "notification-improvements"
    branch TEXT NOT NULL,             -- e.g. "feat/notification-improvements"
    base_branch TEXT NOT NULL,        -- e.g. "main" or "release/2.0"
    status TEXT NOT NULL DEFAULT 'active',  -- active, merged, closed
    created_at TEXT NOT NULL,
    merged_at TEXT,
    UNIQUE(repo_id, name)
);

CREATE TABLE feature_tickets (
    feature_id TEXT NOT NULL REFERENCES features(id),
    ticket_id TEXT NOT NULL REFERENCES tickets(id),
    PRIMARY KEY (feature_id, ticket_id)
);
```

No `parent_feature_id` column is needed — the hierarchy is derived from `base_branch` matching another feature's `branch`. This avoids enforcing a strict tree and allows flexible branch topologies.

### CLI Commands

```bash
# Create a feature branch
conductor feature create <repo> <name> [--from <base>] [--tickets 1262,1263,1264]
# Creates git branch, pushes to origin, records in DB
# --from defaults to repo's default branch (main)
# --tickets optionally links tickets at creation time

# List features for a repo
conductor feature list <repo>
# Shows: name, branch, base, status, worktree count, merged count

# Link/unlink tickets
conductor feature link <repo> <feature-name> --tickets 1265,1266
conductor feature unlink <repo> <feature-name> --tickets 1265

# Create rollup PR (feature branch → base branch)
conductor feature pr <repo> <name> [--draft]
# Equivalent to: gh pr create --head feat/notification-improvements --base main

# Close/archive a feature
conductor feature close <repo> <name>
```

### Worktree Integration

Worktree creation gains a `--feature` flag:

```bash
conductor worktree create <repo> <name> --feature notification-improvements [--ticket 1262]
```

This is equivalent to `--from feat/notification-improvements` but also records the feature association for grouping and progress tracking.

**Explicit opt-in only.** Conductor does not auto-detect feature membership from ticket linkage. The user must specify `--feature` when creating a worktree.

### TUI Integration

#### Branch Picker

When creating a worktree from the TUI, show a branch selection step:

```
Target branch:
  ● main
  ○ feat/notification-improvements (3 worktrees)
  ○ release/2.0 (2 features, 7 worktrees)
```

This replaces the current behavior of always branching from main.

#### Feature Grouping View

In the worktree list, group worktrees under their feature:

```
feat/notification-improvements (3/5 merged)
  ├── fix-1262-blocker-notifs      ✓ merged
  ├── fix-1263-notif-filtering     ✓ merged
  ├── fix-1264-blocked-on          ✓ merged
  ├── fix-1265-tui-gates-panel     ◐ PR open
  └── fix-1266-grouped-notifs      ○ in progress
```

Progress indicator: `{merged_count}/{total_count} merged`

### Workflow Integration

Workflows can accept a `--feature` parameter at invocation:

```bash
conductor workflow run ticket-to-pr --ticket 1262 --feature notification-improvements
```

This sets the base branch for any worktree created during the workflow run. The workflow DSL itself does not need to change — the feature context flows through as the base branch for `worktree create` calls within the workflow.

### Merge Strategy

The rollup PR's merge strategy (squash vs. merge commit) is a team/repo preference. This can be configured:

```toml
# ~/.conductor/config.toml
[defaults]
feature_merge_strategy = "merge"  # or "squash" — default: "merge"
```

Or per-feature at PR creation time:

```bash
conductor feature pr <repo> <name> --squash
```

---

## Decisions Made

1. **Purely local** — no sync with GitHub milestones, Jira epics, or external grouping. Conductor stays ticketing-system agnostic.

2. **Explicit opt-in** — `--feature` flag required when creating worktrees. No auto-detection from ticket linkage.

3. **Single concept** — features and releases use the same mechanism. A release branch is just a feature whose children are other features. No schema distinction.

4. **Hierarchy via base_branch** — no `parent_feature_id`. The tree is derived from branch targeting relationships. Keeps the model flexible.

5. **Merge strategy is configurable** — per-repo default with per-feature override. Teams choose squash or merge commit.

6. **Auto-rebase deferred** — keeping worktrees up to date with the feature branch as sub-PRs merge is a follow-on enhancement. Requires agent intervention for conflict resolution, which ties into the workflow engine but is not needed for the initial implementation.

---

## Follow-up Issues

The following open questions from this RFC have been filed as separate issues:

1. **Feature branch cleanup** — [#1371](https://github.com/devinrosen/conductor-ai/issues/1371): auto-delete branch and worktrees on merge
2. **Stale feature detection** — [#1372](https://github.com/devinrosen/conductor-ai/issues/1372): warn about inactive feature branches
3. **Cross-repo features** — deferred; data model does not preclude it
4. **Auto-rebase on sub-PR merge** — [#1373](https://github.com/devinrosen/conductor-ai/issues/1373): rebase sibling worktrees when one merges

---

## Implementation Order

1. DB migration: `features` and `feature_tickets` tables
2. `FeatureManager` in conductor-core (CRUD, status tracking)
3. CLI commands: `conductor feature create/list/link/pr/close`
4. Worktree integration: `--feature` flag on `worktree create`, store association
5. TUI: branch picker in worktree creation flow
6. TUI: feature grouping view in worktree list
7. Workflow integration: `--feature` parameter on `workflow run`

Steps 1–4 are the core and can land as one PR. Steps 5–7 are independent follow-ups.
