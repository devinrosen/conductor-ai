# Pipeline script tests

Fixture tests for the decision logic in `.conductor/scripts/`: `aggregate-reviews.sh`,
`show-plan-decisions.sh`, the `detect-*.sh` scripts (including `detect-engine-doc-changes.sh`) with `lib-diff-range.sh`, `fmt-fix.sh`,
`submit-review.sh`, `reuse-review.sh`, `review-scope.sh` and `fetch-previous-round.sh` (who counts as a trusted reviewer, with `lib-trusted-review.sh`), `check-pr-ready.sh`, `wait-for-pr-checks.sh`, `post-verification.sh`, `check-summary.sh`, `update-pr-summary.sh` and `squash-merge.sh` (verify-and-merge), `pr-body.sh` (the PR description builder) `verify.sh` (which checks run, its failure signature, and the reruns of changed tests, #3485) and `verify-progress.sh` (whether the implement retry shrank the failures, #3486), plus
`scripts/release/bump.sh` (the release commit), and `lib-branch-home.sh` (when a branch build may open the real conductor database, #3469).
Plain bash (3.2 compatible), plus `jq`, `git`, `cargo`, `rustfmt` and `sqlite3`. CI runs them as the **Pipeline Scripts**
job when `.conductor/scripts/**`, `.conductor/tests/**` or `scripts/release/**` changes.

```bash
.conductor/tests/run.sh                      # everything
.conductor/tests/run.sh test-fmt-fix.sh      # one file
```

The runner unsets `CONDUCTOR_*` and `FLOW_*`, then points `CONDUCTOR_HOME` at an empty temp dir,
so no case reads your real `~/.conductor`; a case that needs a database makes one with
`scratch_conductor_home VERSION` (a `conductor.db` at that schema, with a `marker` row and a
`config.toml`) and passes it as `CONDUCTOR_HOME`. The scratch repos ignore your git config, so
results don't depend on where you run them. The tests assert markers and decisions
(approval, which finding moved), not summary wording.

## Adding a case

Add `case_begin "name"`, run the script with `run_script NAME VAR=value ...`, then assert with
`assert_exit`, `assert_marker`, `assert_no_markers`, `assert_jq '<jq over FLOW_OUTPUT>'`,
`assert_context_contains`, `assert_stderr_contains` or, for scripts that print plain text,
`assert_stdout_contains` / `assert_stdout_not_contains` (see `lib.sh`). New decision logic in a script
needs a case that fails without it.

Beware bash 3.2: a `\"` inside a single-quoted jq program nested in `"$( )"` loses its backslash.
Pass JSON strings with `jq --arg`.

## Stubbing gh

`test-submit-review.sh` puts a stub `gh` first on `PATH` (`write_stub`). Files in `$GH_STUB_DIR`
drive it: `user` (without it `gh api user` fails like a GitHub App token), `author`, `poster`,
`reviews.json` (one JSON array per page; `gh pr review` appends to the last) and `drop_post`. `test-verify-and-merge.sh` has its own stub, driven by `pr.json`, `reviews.json`, `checks.json` (or a `checks.seq/` directory of numbered files consumed one per call, the last repeating) and `issues/N` (the content `FAIL` makes `gh issue view N` fail), which logs `gh pr merge`, `gh pr comment` and `gh issue close` calls, and saves the body file of `gh pr edit` to `edited` (a `edit_fail` file makes it fail).
It strips `--repo`/`-R` and logs every call that came without it (but `repo view`) to `norepo`, so a case asserts the file is empty; a call without `--repo` fails when the cwd is gone, like real `gh`. `views` logs the number of each `gh issue view`, and an `rm_on_merge` file holding a directory makes `gh pr merge` remove it, to simulate a worktree cleaned up under the run. A call
the stub doesn't know fails, so a test can't reach the network. `test-reuse-review.sh` has a smaller stub (`review.json` for one review, `reviews.json` for the list). Copy one for another script that calls `gh`.

## Stubbing cargo

`test-verify.sh` runs `verify.sh` in scratch repos with a stub `cargo` first on `PATH`. The stub logs
its arguments to `$CARGO_STUB_DIR/calls`, and a `fail_build` file there makes `cargo build` fail. Each
scratch repo commits a **fake** `.conductor/tests/run.sh`, which is what verify runs, so the real suite
never recurses into itself. The stub's `run` branch also answers the smoke call
`workflow validate <name> --dry-run` (from `smoke-workflows.sh`) with a JSON pass object (run ID `01TESTPASS`),
or a JSON fail object (`01TESTFAIL`) while a `fail_smoke` file exists.
A `nextest` call with `-E` is a rerun of changed tests (#3485): the stub counts it in `rerun_count` and fails
it when `flaky_on` holds that number (naming the test in `flaky_name`).

`test-smoke-workflows.sh` covers `smoke-workflows.sh` (changed-file and caller detection (transitive) with
`SMOKE_DETECT_ONLY=1`; result classification with a stub `conductor` set through `SMOKE_CONDUCTOR`, driven by
`$SMOKE_STUB_DIR/<workflow>.out` (the JSON the real CLI prints with `--json`) and `.rc`, logging its arguments to `calls`) and `update-pr-smokes.sh`
(a stub `gh` driven by `body.md` and `fail_edit`, writing the edited body to `edited.md`).

## Recording an aggregator fixture

`fixtures/aggregate/recorded-*.json` are `PRIOR_CONTEXTS` rebuilt from a real `review-pr` run,
read-only; `*.expect.json` holds the key fields the script produced (reviewed after the fact).

```bash
sqlite3 -readonly ~/.conductor/conductor.db -json "
  select step_name as step, iteration, context_out as context, markers_out as markers,
         structured_output, result_text
  from workflow_run_steps
  where workflow_run_id = '<run id>' and status = 'completed'
    and (step_name like 'review-%' or step_name in ('validate-workflows','fetch-pr-replies'))
    and step_name != 'review-aggregator' order by position" |
jq '[.[] | .markers = ((.markers // "[]") | fromjson)
     | if .structured_output == null and ((.result_text // "") | test("<<<FLOW_OUTPUT>>>")) then
         .structured_output = (.result_text | capture("<<<FLOW_OUTPUT>>>(?<j>[\\s\\S]*?)<<<END_FLOW_OUTPUT>>>").j | fromjson | tojson)
       else . end | del(.result_text)]' > fixtures/aggregate/recorded-<name>.json
```

Trim reply bodies, and skim for anything private before committing. Only record runs of this repo.
