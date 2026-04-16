# Feature Table Replacement Exploration

Design exploration: can the `features` table and `FeatureManager` be removed, replaced by workflows and/or smarter worktree creation?

This exploration is part of a broader direction: **compact conductor to its core essence**. Remove hardcoded, special-purpose features in favor of general primitives (engine constructs, WorktreeManager) that let workflows and plugins encode the logic. The right product boundary is a small, hard-to-break Rust core paired with a growing library of `.wf` files and agents that users can fork and customize.

This aligns with PHILOSOPHY.md: conductor should be the *platform for building jigs*, not the jig itself. A feature table with its own lifecycle state machine is conductor acting as the jig. Workflows let you encode the jig in files you own.

It also resolves a tension in AUTONOMOUS-SDLC.md, which frames each SDLC stage (pre-flight validation, architecture review, resolution validation) as a new built-in engine step type. Under the compact direction, those stages are workflow patterns — `call quality-gate`, `call validate-resolution` — not engine constructs. The SDLC vision is achievable without adding special-purpose primitives to the engine. Add general power, not special-purpose constructs.

---

## What the feature table currently provides

1. **Lifecycle state machine** — `in_progress → ready_for_review → approved → merged/closed`
2. **Ticket grouping** — `feature_tickets` join table associates multiple tickets with one feature
3. **GitHub Milestone sync** — ties a feature to a milestone and auto-populates tickets
4. **Fan-out execution** (`feature run`) — creates worktrees + spawns headless agents for all tickets, respecting `max_feature_parallelism`
5. **Auto-transitions** — `auto_ready_for_review_if_complete`, `auto_close_if_orphaned` on worktree delete
6. **Staleness tracking** — `last_commit_at` cached per feature branch
7. **Integration PR** — `create_pr()` creates a PR for the feature branch as a whole

> **Context:** The feature table predates the `foreach` workflow construct. Now that `foreach` exists, most of this is redundant.

---

## Option A: `foreach over = tickets` (already exists)

A `process-feature` workflow targeting a worktree (on the feature/release branch) fans out `ticket-to-pr` over its tickets.

```
workflow process-feature {
  meta {
    targets = ["worktree"]
  }

  foreach feature-tickets {
    over          = tickets
    scope         = { label = "release-0.5.2" }
    ordered       = true
    max_parallel  = 3
    workflow      = "ticket-to-pr"
    inputs        = { ticket_id = "{{item.id}}" }
    on_child_fail = skip_dependents
  }
}
```

### What this replaces

| Feature capability | Workflow equivalent |
|---|---|
| Fan-out execution | `foreach over = tickets` |
| Dependency ordering | `ordered = true` + `ticket_dependencies` graph |
| Parallelism control | `max_parallel` |
| Lifecycle tracking | Workflow run status (`running/waiting/completed/failed`) |
| Resumability | Engine reconstructs from `workflow_run_step_fan_out_items` — more robust than orphan reaper |
| Ticket grouping | `scope = { label = "..." }` instead of `feature_tickets` join table |
| ready_for_review gate | `gate human_review` or `gate pr_approval` |

### Gaps

- **Milestone sync** — no `scope = { milestone = "..." }`. Workaround: a pre-step `script` that syncs milestone → label.
- **Integration PR** — needs a final `call create-feature-pr` agent step. Trivial.
- **TUI Features tab** — would become a filtered workflow runs view.
- **Staleness tracking** — `last_commit_at` is lost. Inferrable from git directly; minor UI concern.

---

## Option B: `foreach over = worktrees` (new construct)

When worktrees are **pre-created** by the user (as in a release branch setup), iterating over tickets is the wrong primitive — the worktrees already exist. A `foreach over = worktrees` construct would target existing worktrees directly.

```
workflow process-release {
  meta {
    targets = ["worktree"]
  }

  foreach release-worktrees {
    over          = worktrees
    scope         = { base_branch = "{{worktree.branch}}" }
    ordered       = true
    max_parallel  = 1
    workflow      = "ticket-to-pr"
    on_child_fail = skip_dependents
  }
}
```

Ordering would derive from the same `ticket_dependencies` graph, pivoting through `worktree.ticket_id`.

The first step of `ticket-to-pr` would be `call rebase-worktree` (or a script step running `git rebase origin/<base_branch>`). Since `ordered = true` holds a worktree until its deps complete, the rebase always picks up the previously merged PR.

`max_parallel = 1` for a linear stacked chain; higher values work for independent tickets on the same base.

**Implementation cost:** ~200-300 lines of Rust + a migration for `{{item.*}}` field set (`worktree_slug`, `worktree_path`, `ticket_id`, `branch`, `base_branch`).

---

## Option C: Parent/child worktrees based on ticket dependencies (new WorktreeManager feature)

The most git-native approach. The dependency structure is expressed in the branch hierarchy itself — no workflow ordering logic needed.

### Branch structure

```
main
└── release/0.5.2
    ├── feat/2172-collect-file          (no deps → base: release/0.5.2)
    ├── feat/2173-verify-review         (deps: 2172 → base: feat/2172-collect-file)
    ├── feat/2174-create-gh-issue       (deps: 2173 → base: feat/2173-verify-review)
    └── feat/2175-convert-comment       (no deps → base: release/0.5.2)
```

Each PR targets its parent worktree's branch. The diff for each PR shows only that ticket's changes.

### What already exists

- `worktrees.base_branch` stores which branch a worktree was created from
- `ticket_dependencies` has the dep edges  
- `WorktreeCreateOptions.from_branch` lets you specify the base

### What's missing

1. `worktree create` resolving dep graph → finding parent ticket's worktree → using its branch as `from_branch` automatically
2. A `create_from_dep_graph(root_ticket_id, root_branch)` method on `WorktreeManager` — topological sort over ticket deps, creating worktrees one level at a time
3. `ticket-to-pr` creating PRs against `worktree.base_branch` (not the repo default) — may already work if the agent reads this field

### Comparison: Option B vs Option C

| | Option B: foreach over worktrees | Option C: stacked branches |
|---|---|---|
| Dep structure lives in | workflow DB | git branch graph |
| PRs target | feature/release branch | parent ticket's branch |
| Reviewer sees | accumulated diff | isolated ticket diff |
| Rebase complexity | rebase-worktree step in workflow | `git rebase` on parent branch; cascades through children |
| Works without workflow engine | no | yes |
| Best for | independent parallel tickets | linear dependent ticket chains |

### Tradeoff

Stacked branches are harder to maintain when a parent is amended after review — all children need rebasing. Ordered foreach with a flat base branch sidesteps this because all PRs target the same branch.

For a release branch with sequential linear deps, stacked branching is cleaner. For independent parallel tickets on a shared base, flat targeting + ordered dispatch is simpler.

---

## Recommendation

These options aren't mutually exclusive:

- **Remove the feature table** — it's superseded by `foreach over = tickets` for the fan-out case
- **Add `foreach over = worktrees`** — for the "user pre-creates worktrees" release workflow pattern
- **Add `WorktreeManager::create_from_dep_graph`** — for the stacked PR pattern when tickets have explicit dep relationships

The "feature" concept dissolves. A release branch worktree + a workflow run IS the feature. The lifecycle is the workflow run status. The ticket grouping is a label or branch scope. The dependency ordering is handled natively by the engine or by the git branch structure.

---

## What "compact to core" means in practice

| Current | Compact direction |
|---|---|
| `FeatureManager` + feature table | `process-feature.wf` workflow |
| `feature run` fan-out in Rust | `foreach over = tickets` or `foreach over = worktrees` |
| TUI Features tab | TUI Workflows tab (already exists) |
| AUTONOMOUS-SDLC "new step types" | Workflow templates using existing primitives |
| Hardcoded lifecycle transitions | `gate human_review` / `gate pr_approval` |

**Engine additions that are worth making** are general primitives that increase expressive power for all workflows:
- `foreach over = worktrees` — targets pre-created worktrees by base branch
- `WorktreeManager::create_from_dep_graph` — stacked branch setup from ticket dep graph
- Better `script` step ergonomics for milestone → label sync and similar glue

**Engine additions that are not worth making** are special-purpose constructs that encode a specific workflow in Rust instead of in a `.wf` file:
- New step types for SDLC stages (`pre_flight`, `validate_resolution`, etc.)
- Any manager whose behavior could be expressed as a workflow
- Lifecycle state machines that replicate what workflow run status already provides

The test: if removing the code would just mean writing a `.wf` file instead, the code shouldn't exist.
