#!/usr/bin/env bash
# verify.sh: which checks run for which changed files, with a stub cargo and a
# fake .conductor/tests/run.sh in each scratch repo (never the real suite).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

STUB_DIR=$(scratch_dir)
export CARGO_STUB_DIR="$STUB_DIR"
mkdir -p "$STUB_DIR/bin"
cat > "$STUB_DIR/bin/cargo" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$CARGO_STUB_DIR/calls"
case "$1" in
  build)
    # bump_schema: the build "migrates" the real database, like a caller that
    # ran the branch build without isolation (#3469).
    if [ -f "$CARGO_STUB_DIR/bump_schema" ]; then
      sqlite3 "$CONDUCTOR_HOME/conductor.db" "UPDATE _conductor_meta SET value = '90' WHERE key = 'schema_version'"
    fi
    if [ -f "$CARGO_STUB_DIR/fail_build" ]; then
      # build_errors: the file's content is the compiler output (several errors)
      if [ -f "$CARGO_STUB_DIR/build_errors" ]; then
        cat "$CARGO_STUB_DIR/build_errors"
      else
        echo "error: stub build failure"
      fi
      exit 1
    fi
    ;;
  nextest)
    # A call with -E is a rerun of changed tests (#3485): count it, and fail on the
    # rerun numbered in flaky_on, naming the test in flaky_name.
    case "$*" in
      *" -E "*)
        n=$(( $(cat "$CARGO_STUB_DIR/rerun_count" 2>/dev/null || echo 0) + 1 ))
        echo "$n" > "$CARGO_STUB_DIR/rerun_count"
        if [ -f "$CARGO_STUB_DIR/flaky_on" ] && [ "$(cat "$CARGO_STUB_DIR/flaky_on")" = "$n" ]; then
          echo "        FAIL [   0.1s] conductor-core m::tests::$(cat "$CARGO_STUB_DIR/flaky_name" 2>/dev/null || echo new_test)"
          echo "assertion \`left == right\` failed"
          exit 100
        fi
        if [ -f "$CARGO_STUB_DIR/empty_filter" ]; then
          case "$*" in *--no-tests=pass*) exit 0 ;; esac
          echo "error: no tests to run"
          exit 4
        fi
        exit 0
        ;;
    esac
    # fail_nextest: the file's content is the nextest output; exit non-zero
    if [ -f "$CARGO_STUB_DIR/fail_nextest" ]; then
      cat "$CARGO_STUB_DIR/fail_nextest"
      exit 100
    fi
    ;;
  run)
    case "$*" in
      *--dry-run*)
        # workflow validate <name> --dry-run: the smoke call from smoke-workflows.sh
        name=$(printf '%s\n' "$*" | sed -n -E 's/.* validate ([^ ]+) --dry-run.*/\1/p')
        if [ -f "$CARGO_STUB_DIR/fail_smoke" ]; then
          echo "{\"workflow\":\"${name}\",\"status\":\"fail\",\"run_id\":\"01TESTFAIL\",\"stopped_at\":{\"workflow\":\"${name}\",\"step\":\"s\",\"kind\":\"failed\"},\"warnings\":[],\"error\":\"stub smoke failure\",\"missing_inputs\":[]}"
          exit 1
        fi
        echo "{\"workflow\":\"${name}\",\"status\":\"pass\",\"run_id\":\"01TESTPASS\",\"stopped_at\":{\"workflow\":\"${name}\",\"step\":\"a\",\"kind\":\"agent\"},\"warnings\":[],\"error\":null,\"missing_inputs\":[]}"
        ;;
      *)
        # validate_fail: the file's content is the validate output; exit non-zero
        if [ -f "$CARGO_STUB_DIR/validate_fail" ]; then
          cat "$CARGO_STUB_DIR/validate_fail"
          exit 1
        fi
        echo "${CONDUCTOR_HOME:-}|${CONDUCTOR_DB_PATH-unset}" > "$CARGO_STUB_DIR/validate_env"
        echo "1/1 workflow(s) passed" ;;
    esac
    ;;
esac
exit 0
STUB
chmod +x "$STUB_DIR/bin/cargo"
export PATH="$STUB_DIR/bin:$PATH"

# new_repo FAKE_RUN_BODY — scratch repo on main with a committed fake run.sh;
# switches to a feature branch and cd's into the repo.
new_repo() {
  repo=$(scratch_repo)
  rm -f "$STUB_DIR/fail_build" "$STUB_DIR/fail_smoke" "$STUB_DIR/calls" "$STUB_DIR/bump_schema" "$STUB_DIR/validate_env" "$STUB_DIR/validate_fail" "$STUB_DIR/build_errors" "$STUB_DIR/fail_nextest" "$STUB_DIR/rerun_count" "$STUB_DIR/flaky_on" "$STUB_DIR/flaky_name" "$STUB_DIR/empty_filter"
  commit_file "$repo" .conductor/tests/run.sh "#!/usr/bin/env bash
touch \"\$PWD/ran-sentinel\"
$1" init
  git -C "$repo" checkout -q -b feature
  cd "$repo" || die "cd failed"
}

PASSING='echo "PASS  a"; echo; echo "passed: 3, failed: 0"; exit 0'
FAILING='echo "PASS  a"; echo "FAIL  some case name"; echo "      expected 1, got 2"; echo; echo "passed: 1, failed: 1"; exit 1'

check_status() { # prints status of the pipeline script tests check, or "none"
  flow_output | jq -r '[.checks[] | select(.name == "pipeline script tests") | .status][0] // "none"'
}
build_status() {
  flow_output | jq -r '[.checks[] | select(.name == "cargo build") | .status][0] // "none"'
}

case_begin "script change runs the check and passes with the summary line"
new_repo "$PASSING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_exit 0
assert_no_markers
assert_eq "pass" "$(check_status)" "check status"
assert_jq '[.checks[] | select(.name == "pipeline script tests") | .summary][0] | contains("passed: 3")'

case_begin "tests change runs the check"
new_repo "$PASSING"
commit_file "$repo" .conductor/tests/test-x.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "pass" "$(check_status)" "check status"

case_begin "failing fixture: has_failures with the case name"
new_repo "$FAILING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_eq "fail" "$(check_status)" "check status"
assert_context_contains "some case name"

case_begin "runs even when cargo build fails"
new_repo "$PASSING"
touch "$STUB_DIR/fail_build"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "fail" "$(build_status)" "build status"
assert_eq "pass" "$(check_status)" "check status"

case_begin "Rust-only change does not run it"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/lib.rs "pub fn f() {}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "none" "$(check_status)" "check status"
[ ! -e "$repo/ran-sentinel" ] || fail "run.sh ran for a Rust-only change"

case_begin "other .conductor/ change does not run it"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "none" "$(check_status)" "check status"
assert_jq '[.checks[] | select(.name == "conductor workflow validate")] | length == 1'
[ ! -e "$repo/ran-sentinel" ] || fail "run.sh ran for a .wf-only change"

smoke_status() { # status of the workflow smokes check, or "none"
  flow_output | jq -r '[.checks[] | select(.name == "workflow smokes") | .status][0] // "none"'
}

case_begin "a .wf change runs the smokes and records the run ID"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_exit 0
assert_no_markers
assert_eq "pass" "$(smoke_status)" "smokes status"
assert_jq '.smokes | length == 1 and .[0].workflow == "x" and .[0].status == "pass" and .[0].run_id == "01TESTPASS"'

case_begin "a failing smoke is a failed check and names the workflow and run"
new_repo "$PASSING"
touch "$STUB_DIR/fail_smoke"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_eq "fail" "$(smoke_status)" "smokes status"
assert_context_contains "x (changed) run 01TESTFAIL"
assert_jq '.smokes[0].status == "fail"'

case_begin "scripts-only change runs no smokes"
new_repo "$PASSING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "none" "$(smoke_status)" "smokes status"
assert_jq '.smokes == []'

case_begin "a .wft change smokes the same-name workflow"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" base
commit_file "$repo" conductor-core/templates/x.wft "template" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "pass" "$(smoke_status)" "smokes status"
assert_jq '.smokes[0].workflow == "x"'

case_begin "smokes are skipped when cargo build fails"
new_repo "$PASSING"
touch "$STUB_DIR/fail_build"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "skipped" "$(smoke_status)" "smokes status"
assert_jq '.smokes == []'

# --- the real database (#3469) -------------------------------------------------

case_begin "static validate runs in the caller's home and leaves an older-schema database unchanged"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/db/migrations.rs "pub const LATEST_SCHEMA_VERSION: u32 = 90;" migration
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
home=$(scratch_conductor_home 89)
before=$(cksum < "$home/conductor.db")
run_script verify.sh FEATURE_BASE_BRANCH=main CONDUCTOR_HOME="$home" CONDUCTOR_DB_PATH="$home/conductor.db"
assert_jq '[.checks[] | select(.name == "conductor workflow validate") | .status][0] == "pass"'
assert_eq "$home" "$(cut -d'|' -f1 < "$STUB_DIR/validate_env")" "CONDUCTOR_HOME for validate"
assert_eq "$home/conductor.db" "$(cut -d'|' -f2 < "$STUB_DIR/validate_env")" "CONDUCTOR_DB_PATH for validate"
assert_eq "89" "$(sqlite3 -readonly "$home/conductor.db" "SELECT value FROM _conductor_meta WHERE key = 'schema_version'")" "schema_version"
assert_eq "$before" "$(cksum < "$home/conductor.db")" "database file checksum"
assert_jq '[.checks[] | select(.name == "conductor database schema")] | length == 0'

case_begin "validate failure with no FAIL line shows the log tail (schema newer than the build)"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
printf 'Error: database schema version 91 is newer than this binary supports (90)\n' > "$STUB_DIR/validate_fail"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_jq '[.checks[] | select(.name == "conductor workflow validate")][0].status == "fail"'
assert_context_contains "schema version 91 is newer than this binary"

case_begin "validate failure with FAIL lines shows them, not the log tail"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
printf 'FAIL  x: bad step\nnoise-tail-line\n' > "$STUB_DIR/validate_fail"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_context_contains "FAIL  x: bad step"
assert_jq '(.context | contains("noise-tail-line")) | not'

case_begin "validate-workflows runs validate in the caller's home"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
home=$(scratch_conductor_home 89)
run_script validate-workflows.sh BASE_BRANCH=main CONDUCTOR_HOME="$home"
assert_exit 0
assert_jq '.approved == true'
assert_eq "$home" "$(cut -d'|' -f1 < "$STUB_DIR/validate_env")" "CONDUCTOR_HOME for validate"

case_begin "tripwire: verify fails when the real schema changes during the run"
new_repo "$PASSING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
home=$(scratch_conductor_home 89)
touch "$STUB_DIR/bump_schema"
run_script verify.sh FEATURE_BASE_BRANCH=main CONDUCTOR_HOME="$home"
assert_marker has_failures
assert_jq '[.checks[] | select(.name == "conductor database schema")][0] | .status == "fail" and (.summary | contains("89 to 90"))'
assert_context_contains "refuses to open"

case_begin "tripwire: quiet and unrecorded when the schema is unchanged"
new_repo "$PASSING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main CONDUCTOR_HOME="$(scratch_conductor_home 89)"
assert_no_markers
assert_jq '[.checks[] | select(.name == "conductor database schema")] | length == 0'

case_begin "signature: several compile errors count one each, and nothing else runs"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/lib.rs "pub fn f() {}" change
touch "$STUB_DIR/fail_build"
cat > "$STUB_DIR/build_errors" <<'ERR'
error[E0382]: borrow of moved value: `a`
  --> conductor-core/src/x.rs:10:5
error[E0382]: borrow of moved value: `b`
  --> conductor-core/src/x.rs:20:5
error[E0382]: borrow of moved value: `a`
  --> conductor-core/src/y.rs:30:9
error: aborting due to 3 previous errors
error: could not compile `conductor-core` (lib) due to 3 previous errors
ERR
run_script verify.sh FEATURE_BASE_BRANCH=main VERIFY_CRATES=conductor-core
assert_marker has_failures
assert_jq '.failure_count == 3 and (.failures | length) == 3'
assert_jq '.failures | all(startswith("cargo build: error[E0382]"))'
assert_jq '.failures | any(contains("conductor-core/src/y.rs:30:9"))'

case_begin "signature: failing tests count one each"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/lib.rs "pub fn f() {}" change
printf '        FAIL [   0.1s] conductor-core mod::test_a\n        FAIL [   0.1s] conductor-core mod::test_b\n     Summary [   1s] 5 tests run: 3 passed, 2 failed\n' > "$STUB_DIR/fail_nextest"
run_script verify.sh FEATURE_BASE_BRANCH=main VERIFY_CRATES=conductor-core
assert_marker has_failures
assert_jq '.failure_count == 2'
assert_jq '.failures == ["cargo nextest -p conductor-core: conductor-core mod::test_a", "cargo nextest -p conductor-core: conductor-core mod::test_b"]'

case_begin "signature: a pass is empty"
new_repo "$PASSING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_no_markers
assert_jq '.failures == [] and .failure_count == 0'

case_begin "signature: a skipped run is empty"
new_repo "$PASSING"
commit_file "$repo" README.md "hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_jq '.failures == [] and .failure_count == 0'

case_begin "signature: a failing script test case is one entry"
new_repo "$FAILING"
commit_file "$repo" .conductor/scripts/foo.sh "echo hi" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_jq '.failures == ["pipeline script tests: some case name"] and .failure_count == 1'

case_begin "signature: a failure with nothing to parse is the check name"
new_repo "$PASSING"
commit_file "$repo" .conductor/workflows/x.wf "workflow x {}" change
printf 'Error: something odd\n' > "$STUB_DIR/validate_fail"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_jq '.failures == ["conductor workflow validate"]'

# --- flaky-test rerun (#3485) --------------------------------------------------

flaky_status() { # status of the flaky tests check, or "none"
  flow_output | jq -r '[.checks[] | select(.name == "flaky tests") | .status][0] // "none"'
}
base_on_main() { git -C "$repo" branch -f main HEAD; } # the "base" commit belongs to main, not the diff
rerun_calls() { grep -c -- ' -E ' "$STUB_DIR/calls" 2>/dev/null || true; }
rerun_call_list() { grep -- ' -E ' "$STUB_DIR/calls" 2>/dev/null || true; }

case_begin "flaky: a new test reruns 5 times and passes"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_exit 0
assert_no_markers
assert_eq "pass" "$(flaky_status)" "flaky status"
assert_eq "5" "$(rerun_calls)" "rerun calls"
rerun_call_list | grep -q 'new_test' || fail "rerun expression does not name new_test"
[ "$(rerun_call_list | grep -c -- '--retries 0')" = "5" ] || fail "reruns must pass --retries 0"
assert_jq '[.checks[] | select(.name == "flaky tests") | .summary][0] | contains("passed 5 reruns")'

case_begin "flaky: a test that fails on rerun 3 of 5 fails verify with the test named"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
echo 3 > "$STUB_DIR/flaky_on"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_eq "fail" "$(flaky_status)" "flaky status"
assert_context_contains "new_test"
assert_jq '[.checks[] | select(.name == "flaky tests") | .summary][0] | contains("rerun 3 of 5")'
assert_jq '.failures | index("flaky tests: conductor-core m::tests::new_test") != null'
assert_eq "3" "$(rerun_calls)" "the loop stops at the failing rerun"

case_begin "flaky: an edited line inside an existing test selects only that test"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "#[test]
fn first_test() {
    assert_eq!(1, 1);
}

#[test]
fn second_test() {
    assert_eq!(2, 2);
}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "#[test]
fn first_test() {
    assert_eq!(1, 1);
}

#[test]
fn second_test() {
    assert_eq!(2, 3 - 1);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "pass" "$(flaky_status)" "flaky status"
rerun_call_list | grep -q 'second_test' || fail "second_test not selected"
rerun_call_list | grep -q 'first_test' && fail "first_test must not be selected"

case_begin "flaky: #[tokio::test] with a following attribute is found"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[tokio::test(flavor = \"multi_thread\")]
#[ignore]
async fn async_test() {
    assert!(true);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_eq "pass" "$(flaky_status)" "flaky status"
rerun_call_list | head -n 1 | grep -q 'async_test' || fail "async_test not selected"

case_begin "flaky: a Rust change with no test changes records no check"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "#[test]
fn some_test() {
    assert!(true);
}

fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "#[test]
fn some_test() {
    assert!(true);
}

fn helper() { let _ = 1; }" change
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_exit 0
assert_eq "none" "$(flaky_status)" "flaky status"
assert_eq "0" "$(rerun_calls)" "rerun calls"

case_begin "flaky: more changed tests than the limit is skipped"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn test_a() {
    assert!(true);
}

#[test]
fn test_b() {
    assert!(true);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main VERIFY_RERUN_MAX_TESTS=1
assert_exit 0
assert_no_markers
assert_eq "skipped" "$(flaky_status)" "flaky status"
assert_eq "0" "$(rerun_calls)" "rerun calls"

case_begin "flaky: the time cap stops new reruns and still passes"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main VERIFY_RERUN_BUDGET=0
assert_exit 0
assert_eq "pass" "$(flaky_status)" "flaky status"
assert_eq "1" "$(rerun_calls)" "rerun calls"
assert_jq '[.checks[] | select(.name == "flaky tests") | .summary][0] | contains("time cap")'

case_begin "flaky: a failing main nextest run skips the reruns"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
printf '        FAIL [   0.1s] conductor-core mod::test_a\n     Summary [   1s] 1 tests run: 0 passed, 1 failed\n' > "$STUB_DIR/fail_nextest"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_eq "none" "$(flaky_status)" "flaky status"

case_begin "flaky: a rerun filter that matches no tests fails instead of passing"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
touch "$STUB_DIR/empty_filter"
run_script verify.sh FEATURE_BASE_BRANCH=main
assert_marker has_failures
assert_eq "fail" "$(flaky_status)" "flaky status"
assert_context_contains "matched no tests"

case_begin "flaky: a failing git diff skips the check with a reason, not silently"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
GIT_SHIM=$(scratch_dir)
REAL_GIT=$(command -v git)
cat > "$GIT_SHIM/git" <<SHIM
#!/usr/bin/env bash
case "\$*" in *"diff -U0"*) echo "fatal: simulated diff failure" >&2; exit 128 ;; esac
exec "$REAL_GIT" "\$@"
SHIM
chmod +x "$GIT_SHIM/git"
run_script verify.sh FEATURE_BASE_BRANCH=main "PATH=$GIT_SHIM:$PATH"
assert_eq "skipped" "$(flaky_status)" "flaky status"
assert_jq '[.checks[] | select(.name == "flaky tests") | .summary][0] | contains("git diff failed")'
assert_eq "0" "$(rerun_calls)" "no reruns after a diff failure"

case_begin "flaky: an empty merge base skips the check with a reason, not silently"
new_repo "$PASSING"
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}" base
base_on_main
commit_file "$repo" conductor-core/src/x.rs "fn helper() {}

#[test]
fn new_test() {
    assert!(true);
}" change
run_script verify.sh FEATURE_BASE_BRANCH=main VERIFY_CRATES=conductor-core
assert_eq "skipped" "$(flaky_status)" "flaky status"
assert_jq '[.checks[] | select(.name == "flaky tests") | .summary][0] | contains("no merge base")'
assert_eq "0" "$(rerun_calls)" "no reruns without a merge base"

finish
