# verify-and-merge

Checks that a PR conductor has already reviewed is ready, runs it against its issue's acceptance criteria, posts the evidence on the PR, and squash-merges it. Run it from the PR's worktree:

```bash
conductor workflow run verify-and-merge <repo> <worktree>                    # merges after verification
conductor workflow run verify-and-merge <repo> <worktree> --input confirm=true   # stops at a human gate first
conductor workflow run verify-and-merge <repo> <worktree> --input auto_rereview=false   # stop after a base merge, as before
conductor workflow run verify-and-merge <repo> <worktree> --input wait_for_checks=false   # refuse while required checks are running, as before
```

It ships as the embedded template `verify-and-merge` (`conductor_instantiate_template`); conductor-ai's own copy is `.conductor/workflows/verify-and-merge.wf`.

## Steps

| Step | What it does |
| --- | --- |
| `check-pr-ready` | Refuses unless the PR can be verified and merged (below). Emits `pr_ready` (or `base_merged` or `checks_pending`, below) with `pr_number`, `pr_title`, `base_branch`, `head_sha`, `linked_issues` and `required_checks`. |
| `iterate-pr` | Only after a base merge (this run's or an earlier one's): re-reviews the new head and fixes what it finds. |
| `wait-for-pr-checks` | Only after a base merge (this run's or an earlier one's): polls the required checks on the new head until they pass, one fails, or 45 minutes pass. |
| `wait-for-pending-checks` | Only on `checks_pending`: `wait-for-pr-checks.sh` again, polling until the required checks pass, one fails, or 45 minutes pass. |
| `recheck-after-checks` | Only on `checks_pending`: `check-pr-ready` again, with `AUTO_REREVIEW=false` and `WAIT_FOR_CHECKS=false`. |
| `recheck-pr-ready` | Only after a base merge (this run's or an earlier one's): `check-pr-ready` again, with `AUTO_REREVIEW=false` and `WAIT_FOR_CHECKS=false`, so it confirms the new review is on the head and All Clear. |
| `verify-acceptance` | An agent reads the linked issue's acceptance criteria and the diff, follows `.conductor/verify-recipe.md`, builds and exercises the branch, and returns a pass / fail / not_verifiable verdict with evidence per criterion. It changes nothing. |
| `post-verification` | Refuses if the head moved or the agent left changes in the worktree, posts the report as a PR comment, and stops the run when verification failed. |
| `check-summary` | Decides whether the PR's `## Summary` may be stale (see "The Summary"). Emits `summary_stale` or `summary_current`; with no `## Summary` section it is current. |
| `refresh-summary` | Only on `summary_stale`: an agent (Sonnet) checks the Summary against `origin/<base>...HEAD` and the commits since the PR opened, and returns "still accurate" or a rewrite in the same style. It edits nothing. |
| `update-pr-summary` | Only on `summary_stale`: writes the rewrite into the PR body and sets the head marker. A failed edit stops the run before the merge. |
| `human_review` | Only with `confirm`: a gate before the merge. |
| `squash-merge` | Squash-merges with subject `<title> (#PR)` and a body of the PR's Summary (minus HTML comments and sentences naming a local path: a `/Users/` or `/home/` directory, a `.git/worktrees/` path or a `PLAN.md` path), a `Closes #N` line per linked issue and the commits' `Co-Authored-By` and `Claude-Session` trailers, deduplicated. Pins the head with `--match-head-commit`, then closes any linked issue the merge left open (a merge into a release branch does not close them by itself). |

## The Summary

The squash commit's body comes from the PR's `## Summary`, which `pr-body.sh` writes when the PR opens. Review rounds and hand fixes can change the code afterwards (#3471), so the run refreshes it before the merge, after `post-verification` and before the `human_review` gate, so whoever approves sees the final text.

`push-and-pr.sh` ends the body with a hidden `<!-- conductor-summary-head: SHA -->` comment, the head the Summary was written for. `check-summary` reads it:

- marker equals the head: `summary_current`, so the run goes straight to the merge with no model call
- marker is an ancestor and only merge commits followed (a base merge): `summary_current`
- a non-merge commit followed, or the marker is not an ancestor (rebased, force-pushed): `summary_stale`
- no marker (a hand-opened or `--fill` PR, or one from before this): `summary_stale` when a non-merge commit on `origin/<base>..HEAD` is dated after the PR's `createdAt`
- no `## Summary` section: `summary_current`; `squash-merge` uses the first paragraph as before

`update-pr-summary` moves the marker to the head even when the Summary was still accurate, so a re-run at the same head skips the agent. `squash-merge` re-reads the body and strips HTML comments, so the commit and the PR agree and the marker never reaches a commit.

## Refusals

Each is one line, which becomes the step error:

- no open PR for the branch, or the PR is a draft
- a `release/*` PR into the default branch (those are rebase-merged by hand, then `scripts/release/finish.sh`)
- no `.conductor/verify-recipe.md`
- the local branch is not the PR head
- no conductor review by a trusted author (OWNER, MEMBER, COLLABORATOR, or a bot login listed in `TRUSTED_REVIEW_BOTS`, which conductor-ai sets to `conductor-ai-reviewer[bot]`; not dismissed; the rule lives in `lib-trusted-review.sh`), or the latest one predates the head commit, or it is not All Clear
- the review was posted as a comment because the PR changes the review pipeline, and nobody approved it (approve the PR, or run with `confirm=true`)
- a required check failing, or still running with `wait_for_checks=false` (or at a recheck)
- merge conflicts
- the branch does not contain its base, with `auto_rereview=false`: the base is merged in and pushed, then the run stops so the new head gets reviewed (`iterate-pr`), and you re-run verify-and-merge. With the default, see below; the same refusal applies if the base moves again during the re-review

A `dry_run` of the workflow skips every script step (the engine records "dry-run: script not executed", `runkon-flow` `src/executors/script.rs`), so `check-pr-ready`, `post-verification`, `check-summary` and `squash-merge` never run and nothing is posted, merged or pushed. Their `DRY_RUN=true` branches apply only when you run a script by hand, where it prints what it would do.

## Waiting for CI

A ticket-to-pr run ends before CI does, so an approved head often has required checks still running. With `wait_for_checks` (default `true`), when that is the only blocker `check-pr-ready` emits `checks_pending` instead of refusing (#3480). `wait-for-pending-checks` then polls the required checks (30 s, 45 minutes), and `recheck-after-checks` runs the readiness checks once more with `AUTO_REREVIEW=false` and `WAIT_FOR_CHECKS=false`: no second wait, no auto re-review, so a push, a base move or a new review during the wait refuses. A check that fails while waiting stops the run before `verify-acceptance`, naming the check. A failing check, a stale review, unreviewed commits and conflicts refuse straight away, as before. A head behind its base takes the base-merge path below instead, because the merge replaces the head the pending CI belongs to. `wait_for_checks=false` keeps the old refusal. A dry run skips scripts, so the smoke takes the direct path.

## After a base merge

When the branch doesn't contain its base, `check-pr-ready` merges the base in and pushes. That is checked last, so it happens only when every other check passed. With `auto_rereview` (default `true`) it then emits `base_merged` and the run continues instead of stopping:

1. `iterate-pr` runs on the new head. The re-review is incremental since the last trusted review (#3421).
2. `wait-for-pr-checks` polls `gh pr checks --required` every 30 s, up to 45 minutes (`WAIT_TIMEOUT`, `POLL_INTERVAL`). It waits for the PR head to match the local head and for every required check named before the merge to be present and passing.
3. `recheck-pr-ready` runs the readiness checks once more, with no further auto re-review.
4. Verification and the merge go on as usual.

**A head an earlier run moved.** If a previous run (or a person) already merged the base and pushed, nothing has reviewed that head, so the review predates it, and CI on the merge commit may still be running. With `auto_rereview=true`, `check-pr-ready` treats such a head like a base merge made in this run when every commit since the last trusted review is a merge whose other parents are on `origin/<base>`: the stale review and still-running required checks don't refuse, and it emits `base_merged` for the current head (#3452). The review must still have been All Clear on its own commit, and a failing check still refuses. A head with any other commit since the review (or a rebased history, or a merge of some other branch) still refuses with "predates head", and `auto_rereview=false` keeps both refusals.

The run stops with a one-line reason, leaving the PR unmerged, when `iterate-pr` fails or hits max iterations, a required check fails, the wait times out, or the re-check refuses (the review is not All Clear, the base moved again, the head doesn't match). Only one automatic round happens. If `iterate-pr` reaches its human gate (a pushback it can't resolve), the run pauses there. A run that hits a base move costs a review (about $2-2.50) and up to 45 minutes of waiting.

A workflow dry run skips script steps, so `base_merged` is never set and the smoke run takes the direct path. The re-review block is covered by static validation and `iterate-pr`'s own smoke.

## The recipe

`.conductor/verify-recipe.md` is the repo's own instructions to `verify-acceptance`: how to build the branch, how to exercise each kind of change (CLI, API, UI), what never to touch (production data, real config homes, a migrated database), which pipeline checks to run for changed scripts or workflows, and what a correct release-branch target is. conductor-ai's is the reference. Without it the run refuses before any agent call.

## What it does not do

It does not delete the worktree or branch, because the run executes in that worktree. `squash-merge` prints the cleanup command (`conductor worktree cleanup`). A `release/*` → `main` PR stays manual.

## Adopting it in another repo

1. `conductor_instantiate_template verify-and-merge` into the repo.
2. Copy `check-pr-ready.sh`, `post-verification.sh`, `check-summary.sh`, `update-pr-summary.sh`, `squash-merge.sh`, `lib-gh-retry.sh`, `wait-for-pr-checks.sh`, `lib-git-retry.sh`, `lib-pr-checks.sh` and `lib-trusted-review.sh` from conductor-ai's `.conductor/scripts/`, plus `.conductor/agents/verify-acceptance.md`, `.conductor/agents/refresh-summary.md`, `.conductor/schemas/verify-acceptance.yaml` and `.conductor/schemas/refresh-summary.yaml`. For the marker, take the summary-head lines of `push-and-pr.sh` too; without it `check-summary` uses the `createdAt` fallback.
3. `wait-for-pr-checks.sh` is needed even without `iterate-pr`: the `if check-pr-ready.checks_pending` block uses it too. The base-merge path calls the `iterate-pr` workflow (the `iterate-pr` template). Without one, drop the `if check-pr-ready.base_merged` block and set the `auto_rereview` default to `"false"`.
4. Write `.conductor/verify-recipe.md`, and adapt the `as` identities and, for issues outside GitHub, the issue closing in `squash-merge.sh`.
5. Smoke it: `conductor workflow validate --dry-run verify-and-merge` from a worktree.

The decision logic of the scripts is covered by `.conductor/tests/test-verify-and-merge.sh`.
