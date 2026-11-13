# Pipeline script tests

Fixture tests for the decision logic in `.conductor/scripts/`: `aggregate-reviews.sh`,
`show-plan-decisions.sh`, the `detect-*.sh` scripts with `lib-diff-range.sh`, `fmt-fix.sh`,
`submit-review.sh`, `pr-body.sh` (the PR description builder) and `verify.sh` (which checks run).
Plain bash (3.2 compatible), plus `jq`, `git` and `rustfmt`. CI runs them as the **Pipeline Scripts**
job when `.conductor/scripts/**` or `.conductor/tests/**` changes.

```bash
.conductor/tests/run.sh                      # everything
.conductor/tests/run.sh test-fmt-fix.sh      # one file
```

The runner unsets `CONDUCTOR_*` and `FLOW_*`, and the scratch repos ignore your git config, so
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
`reviews.json` (one JSON array per page; `gh pr review` appends to the last) and `drop_post`. A call
the stub doesn't know fails, so a test can't reach the network. Copy it for another script that calls `gh`.

## Stubbing cargo

`test-verify.sh` runs `verify.sh` in scratch repos with a stub `cargo` first on `PATH`. The stub logs
its arguments to `$CARGO_STUB_DIR/calls`, and a `fail_build` file there makes `cargo build` fail. Each
scratch repo commits a **fake** `.conductor/tests/run.sh`, which is what verify runs, so the real suite
never recurses into itself.

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
