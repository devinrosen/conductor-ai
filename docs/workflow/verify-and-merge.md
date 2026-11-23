# verify-and-merge

Checks that a PR conductor has already reviewed is ready, runs it against its issue's acceptance criteria, posts the evidence on the PR, and squash-merges it. Run it from the PR's worktree:

```bash
conductor workflow run verify-and-merge <repo> <worktree>                    # merges after verification
conductor workflow run verify-and-merge <repo> <worktree> --input confirm=true   # stops at a human gate first
conductor workflow run verify-and-merge <repo> <worktree> --input auto_rereview=false   # stop after a base merge, as before
```

It ships as the embedded template `verify-and-merge` (`conductor_instantiate_template`); conductor-ai's own copy is `.conductor/workflows/verify-and-merge.wf`.

## Steps

| Step | What it does |
| --- | --- |
| `check-pr-ready` | Refuses unless the PR can be verified and merged (below). Emits `pr_ready` (or `base_merged`, below) with `pr_number`, `pr_title`, `base_branch`, `head_sha`, `linked_issues` and `required_checks`. |
| `iterate-pr` | Only after a base merge: re-reviews the new head and fixes what it finds. |
| `wait-for-pr-checks` | Only after a base merge: polls the required checks on the new head until they pass, one fails, or 45 minutes pass. |
| `recheck-pr-ready` | Only after a base merge: `check-pr-ready` again, with `AUTO_REREVIEW=false`, so it confirms the new review is on the head and All Clear. |
| `verify-acceptance` | An agent reads the linked issue's acceptance criteria and the diff, follows `.conductor/verify-recipe.md`, builds and exercises the branch, and returns a pass / fail / not_verifiable verdict with evidence per criterion. It changes nothing. |
| `post-verification` | Refuses if the head moved or the agent left changes in the worktree, posts the report as a PR comment, and stops the run when verification failed. |
| `human_review` | Only with `confirm`: a gate before the merge. |
| `squash-merge` | Squash-merges with subject `<title> (#PR)` and a body of the PR's Summary (minus sentences naming a local path: a `/Users/` or `/home/` directory, a `.git/worktrees/` path or a `PLAN.md` path), a `Closes #N` line per linked issue and the commits' `Co-Authored-By` and `Claude-Session` trailers, deduplicated. Pins the head with `--match-head-commit`, then closes any linked issue the merge left open (a merge into a release branch does not close them by itself). |

## Refusals

Each is one line, which becomes the step error:

- no open PR for the branch, or the PR is a draft
- a `release/*` PR into the default branch (those are rebase-merged by hand, then `scripts/release/finish.sh`)
- no `.conductor/verify-recipe.md`
- the local branch is not the PR head
- no conductor review by a trusted author (OWNER, MEMBER, COLLABORATOR, or a bot login listed in `TRUSTED_REVIEW_BOTS`, which conductor-ai sets to `conductor-ai-reviewer[bot]`; not dismissed; the rule lives in `lib-trusted-review.sh`), or the latest one predates the head commit, or it is not All Clear
- the review was posted as a comment because the PR changes the review pipeline, and nobody approved it (approve the PR, or run with `confirm=true`)
- a required check failing or still running
- merge conflicts
- the branch does not contain its base, with `auto_rereview=false`: the base is merged in and pushed, then the run stops so the new head gets reviewed (`iterate-pr`), and you re-run verify-and-merge. With the default, see below; the same refusal applies if the base moves again during the re-review

A `dry_run` of the workflow skips every script step (the engine records "dry-run: script not executed", `runkon-flow` `src/executors/script.rs`), so `check-pr-ready`, `post-verification` and `squash-merge` never run and nothing is posted, merged or pushed. Their `DRY_RUN=true` branches apply only when you run a script by hand, where it prints what it would do.

## After a base merge

When the branch doesn't contain its base, `check-pr-ready` merges the base in and pushes. That is checked last, so it happens only when every other check passed. With `auto_rereview` (default `true`) it then emits `base_merged` and the run continues instead of stopping:

1. `iterate-pr` runs on the new head. The re-review is incremental since the last trusted review (#3421).
2. `wait-for-pr-checks` polls `gh pr checks --required` every 30 s, up to 45 minutes (`WAIT_TIMEOUT`, `POLL_INTERVAL`). It waits for the PR head to match the local head and for every required check named before the merge to be present and passing.
3. `recheck-pr-ready` runs the readiness checks once more, with no further auto re-review.
4. Verification and the merge go on as usual.

The run stops with a one-line reason, leaving the PR unmerged, when `iterate-pr` fails or hits max iterations, a required check fails, the wait times out, or the re-check refuses (the review is not All Clear, the base moved again, the head doesn't match). Only one automatic round happens. If `iterate-pr` reaches its human gate (a pushback it can't resolve), the run pauses there. A run that hits a base move costs a review (about $2-2.50) and up to 45 minutes of waiting.

A workflow dry run skips script steps, so `base_merged` is never set and the smoke run takes the direct path. The re-review block is covered by static validation and `iterate-pr`'s own smoke.

## The recipe

`.conductor/verify-recipe.md` is the repo's own instructions to `verify-acceptance`: how to build the branch, how to exercise each kind of change (CLI, API, UI), what never to touch (production data, real config homes, a migrated database), which pipeline checks to run for changed scripts or workflows, and what a correct release-branch target is. conductor-ai's is the reference. Without it the run refuses before any agent call.

## What it does not do

It does not delete the worktree or branch, because the run executes in that worktree. `squash-merge` prints the cleanup command (`conductor worktree cleanup`). A `release/*` → `main` PR stays manual.

## Adopting it in another repo

1. `conductor_instantiate_template verify-and-merge` into the repo.
2. Copy `check-pr-ready.sh`, `post-verification.sh`, `squash-merge.sh`, `lib-gh-retry.sh`, `wait-for-pr-checks.sh`, `lib-git-retry.sh`, `lib-pr-checks.sh` and `lib-trusted-review.sh` from conductor-ai's `.conductor/scripts/`, plus `.conductor/agents/verify-acceptance.md` and `.conductor/schemas/verify-acceptance.yaml`.
3. The base-merge path calls the `iterate-pr` workflow (the `iterate-pr` template). Without one, drop the `if check-pr-ready.base_merged` block and set the `auto_rereview` default to `"false"`.
4. Write `.conductor/verify-recipe.md`, and adapt the `as` identities and, for issues outside GitHub, the issue closing in `squash-merge.sh`.
5. Smoke it: `conductor workflow validate --dry-run verify-and-merge` from a worktree.

The decision logic of the four scripts is covered by `.conductor/tests/test-verify-and-merge.sh`.
