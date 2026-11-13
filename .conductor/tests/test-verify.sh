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
    if [ -f "$CARGO_STUB_DIR/fail_build" ]; then
      echo "error: stub build failure"
      exit 1
    fi
    ;;
  run) echo "1/1 workflow(s) passed" ;;
esac
exit 0
STUB
chmod +x "$STUB_DIR/bin/cargo"
export PATH="$STUB_DIR/bin:$PATH"

# new_repo FAKE_RUN_BODY — scratch repo on main with a committed fake run.sh;
# switches to a feature branch and cd's into the repo.
new_repo() {
  repo=$(scratch_repo)
  rm -f "$STUB_DIR/fail_build" "$STUB_DIR/calls"
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

finish
