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
      echo "error: stub build failure"
      exit 1
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
  rm -f "$STUB_DIR/fail_build" "$STUB_DIR/fail_smoke" "$STUB_DIR/calls" "$STUB_DIR/bump_schema" "$STUB_DIR/validate_env"
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

finish
