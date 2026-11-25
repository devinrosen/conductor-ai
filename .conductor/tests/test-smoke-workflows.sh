#!/usr/bin/env bash
# smoke-workflows.sh (which workflows get a dry-run smoke, how results are classified)
# and update-pr-smokes.sh (the PR body's ## Smokes section), #3453.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# --- smoke-workflows.sh ----------------------------------------------------------

# new_repo — a scratch repo with workflows: child (called by parent, in two call
# forms), parent (called by grand), grand, child-x (not called by parent), other, and
# the templates child.wft and orphan.wft. cd's into it.
new_repo() {
  repo=$(scratch_repo)
  commit_file "$repo" .conductor/workflows/child.wf "workflow child {
  do {
    script a { run = \"a.sh\" }
  }
}" init
  commit_file "$repo" .conductor/workflows/parent.wf "workflow parent {
  do {
    call workflow child
    call workflow child {
      inputs = {}
    }
  }
}" parent
  commit_file "$repo" .conductor/workflows/grand.wf "workflow grand {
  do {
    call workflow parent
  }
}" grand
  commit_file "$repo" .conductor/workflows/child-x.wf "workflow child-x {
  do {
    script a { run = \"a.sh\" }
  }
}" childx
  commit_file "$repo" .conductor/workflows/other.wf "workflow other {
  do {
    call workflow child-x
  }
}" other
  commit_file "$repo" conductor-core/templates/child.wft "t" tmpl
  commit_file "$repo" conductor-core/templates/orphan.wft "t" tmpl2
  cd "$repo" || die "cd failed"
}

# detect PATH... — the targets for the given changed paths.
detect() {
  STDOUT=$(printf '%s\n' "$@" | SMOKE_DETECT_ONLY=1 bash "$SCRIPTS/smoke-workflows.sh" 2>&1)
  EXIT=$?
}

case_begin "changed child pulls in its direct parent only, not a name that merely starts with it"
new_repo
detect .conductor/workflows/child.wf
assert_exit 0
assert_eq "child changed
parent calls child" "$STDOUT" "targets"

case_begin "changed child-x does not pull in parent"
new_repo
detect .conductor/workflows/child-x.wf
assert_eq "child-x changed
other calls child-x" "$STDOUT" "targets"

case_begin "a changed template maps to the same-name workflow and its parent"
new_repo
detect conductor-core/templates/child.wft
assert_eq "child changed
parent calls child" "$STDOUT" "targets"

case_begin "a template with no workflow is skipped by name"
new_repo
detect conductor-core/templates/orphan.wft
assert_stdout_contains "skip template orphan"
assert_stdout_not_contains "changed"

case_begin "parent and child both changed are each listed once"
new_repo
detect .conductor/workflows/child.wf .conductor/workflows/parent.wf .conductor/workflows/child.wf
assert_eq "child changed
parent changed
grand calls parent" "$STDOUT" "targets"

case_begin "non-pipeline changes give no targets"
new_repo
detect conductor-core/src/lib.rs README.md .conductor/scripts/foo.sh .conductor/tests/test-x.sh
assert_exit 0
assert_eq "" "$STDOUT" "targets"

case_begin "a deleted workflow and a subdirectory file are ignored"
new_repo
detect .conductor/workflows/gone.wf .conductor/workflows/sub/child.wf
assert_eq "" "$STDOUT" "targets"

case_begin "the name on the workflow line wins over the file stem"
new_repo
commit_file "$repo" .conductor/workflows/stem.wf "workflow real-name {
}" stem
detect .conductor/workflows/stem.wf
assert_eq "real-name changed" "$STDOUT" "targets"

# --- stub conductor -----------------------------------------------------------------
# The stub logs its args and prints the contents of $SMOKE_STUB_DIR/<workflow>.out
# (default: a JSON pass object), exiting with <workflow>.rc (default 0).
STUB_DIR=$(scratch_dir)
export SMOKE_STUB_DIR="$STUB_DIR"
cat > "$STUB_DIR/conductor" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$SMOKE_STUB_DIR/calls"
echo "${CONDUCTOR_HOME:-}|${CONDUCTOR_DB_PATH-unset}" > "$SMOKE_STUB_DIR/env"
[ -f "$CONDUCTOR_HOME/conductor.db" ] && sqlite3 -readonly "$CONDUCTOR_HOME/conductor.db" "SELECT v FROM marker" > "$SMOKE_STUB_DIR/marker"
name="$3"
[ -f "$SMOKE_STUB_DIR/drain" ] && cat > /dev/null
if [ -f "$SMOKE_STUB_DIR/$name.out" ]; then cat "$SMOKE_STUB_DIR/$name.out"; else
  echo "{\"workflow\":\"$name\",\"status\":\"pass\",\"run_id\":\"01RUN$name\",\"stopped_at\":{\"workflow\":\"$name\",\"step\":\"a\",\"kind\":\"agent\"},\"warnings\":[],\"error\":null,\"missing_inputs\":[]}"
fi
[ -f "$SMOKE_STUB_DIR/$name.rc" ] && exit "$(cat "$SMOKE_STUB_DIR/$name.rc")"
exit 0
STUB
chmod +x "$STUB_DIR/conductor"

# stub_reset — forget calls and canned output from the previous case.
stub_reset() {
  find "$STUB_DIR" -maxdepth 1 \( -name calls -o -name drain -o -name env -o -name marker -o -name '*.out' -o -name '*.rc' \) -type f -delete
}

# stub_result WORKFLOW RC LINE — canned conductor output and exit code for one workflow.
stub_result() {
  printf '%s\n' "$3" > "$STUB_DIR/$1.out"
  echo "$2" > "$STUB_DIR/$1.rc"
}

# smoke PATH... — run with the stub; set STDOUT and EXIT.
smoke() {
  STDOUT=$(printf '%s\n' "$@" | SMOKE_CONDUCTOR="$STUB_DIR/conductor" bash "$SCRIPTS/smoke-workflows.sh" 2>/dev/null)
  EXIT=$?
}
smokes_jq() { printf '%s' "$STDOUT" | jq -e "$1" >/dev/null 2>&1 || fail "jq check failed: $1; stdout: $STDOUT"; }

case_begin "passing smokes carry the run ID and the reason"
new_repo
stub_reset
smoke .conductor/workflows/child.wf
assert_exit 0
smokes_jq '.smokes | length == 2'
smokes_jq '.smokes[0] | .workflow == "child" and .reason == "changed" and .status == "pass" and .run_id == "01RUNchild"'
smokes_jq '.smokes[1] | .workflow == "parent" and .reason == "calls child" and .status == "pass"'
assert_eq "workflow validate child --dry-run --json --path .
workflow validate parent --dry-run --json --path ." "$(cat "$STUB_DIR/calls")" "conductor calls"

case_begin "a missing required input is a skip naming the input, not a failure"
new_repo
stub_reset
stub_result parent 1 '{"workflow":"parent","status":"fail","run_id":null,"stopped_at":null,"warnings":[],"error":"Missing required input: '"'ticket_id'"' (no ticket linked)","missing_inputs":["ticket_id"]}'
smoke .conductor/workflows/parent.wf
assert_exit 0
smokes_jq '.smokes[0] | .workflow == "parent" and .status == "skip" and (.detail | contains("ticket_id"))'

case_begin "any other failed status fails the script and keeps the run ID and error"
new_repo
stub_reset
stub_result child 1 '{"workflow":"child","status":"fail","run_id":"01RUNBAD","stopped_at":{"workflow":"child","step":"s","kind":"failed"},"warnings":[],"error":"boom","missing_inputs":[]}'
smoke .conductor/workflows/child.wf
assert_exit 1
smokes_jq '.smokes[0] | .status == "fail" and .run_id == "01RUNBAD" and (.detail | contains("boom"))'
smokes_jq '.smokes[1].status == "pass"'

case_begin "a static failure object fails"
new_repo
stub_reset
stub_result child 1 '{"workflow":"child","status":"fail","run_id":null,"stopped_at":null,"warnings":[],"error":"unknown agent","missing_inputs":[]}'
smoke .conductor/workflows/child-x.wf .conductor/workflows/child.wf
assert_exit 1
smokes_jq '[.smokes[] | select(.workflow == "child")][0] | .status == "fail" and (.detail | contains("unknown agent"))'

case_begin "output with no JSON fails and keeps the text"
new_repo
stub_reset
stub_result child 1 "error: could not compile"
smoke .conductor/workflows/child.wf
assert_exit 1
smokes_jq '.smokes[0] | .status == "fail" and (.detail | contains("could not compile"))'

case_begin "a status skip object is a skip with its error as the detail"
new_repo
stub_reset
stub_result child 0 '{"workflow":"child","status":"skip","run_id":null,"stopped_at":null,"warnings":[],"error":"needs a worktree","missing_inputs":[]}'
smoke .conductor/workflows/child-x.wf
assert_exit 0
smokes_jq '[.smokes[] | select(.workflow == "child-x")][0] | .status == "pass"'
smoke .conductor/workflows/child.wf
smokes_jq '.smokes[0] | .status == "skip" and (.detail | contains("needs a worktree"))'

case_begin "a pass with warnings puts them in the detail"
new_repo
stub_reset
stub_result child 0 '{"workflow":"child","status":"pass","run_id":"01RUNW","stopped_at":null,"warnings":["env E uses input opt"],"error":null,"missing_inputs":[]}'
smoke .conductor/workflows/child.wf
assert_exit 0
smokes_jq '.smokes[0] | .status == "pass" and .run_id == "01RUNW" and (.detail | contains("ran to completion") and contains("env E uses input opt"))'

case_begin "a human SMOKE PASS line alone is not read: it fails"
new_repo
stub_reset
stub_result child 0 "SMOKE PASS  child  stopped at agent step 'a'  (run 01RUNTXT)"
smoke .conductor/workflows/child.wf
assert_exit 1
smokes_jq '.smokes[0] | .status == "fail"'

case_begin "no pipeline change: empty list, exit 0, no conductor call"
new_repo
stub_reset
smoke conductor-core/src/lib.rs
assert_exit 0
assert_eq '{"smokes":[]}' "$STDOUT" "output"
[ ! -e "$STUB_DIR/calls" ] || fail "conductor was called with nothing to smoke"

case_begin "a workflow in the inputs table gets its --input arg"
new_repo
stub_reset
commit_file "$repo" .conductor/workflows/analyze-ux.wf "workflow analyze-ux {
}" ux
smoke .conductor/workflows/analyze-ux.wf
assert_exit 0
assert_eq "workflow validate analyze-ux --dry-run --json --path . --input repo=conductor-ai" "$(cat "$STUB_DIR/calls")" "conductor calls"

case_begin "an orphan template is a named skip and makes no call"
new_repo
stub_reset
smoke conductor-core/templates/orphan.wft
assert_exit 0
smokes_jq '.smokes | length == 1 and .[0].status == "skip" and (.[0].workflow | contains("orphan"))'
[ ! -e "$STUB_DIR/calls" ] || fail "conductor was called for an orphan template"

case_begin "a conductor that drains stdin does not drop the remaining targets"
new_repo
stub_reset
touch "$STUB_DIR/drain"
smoke .conductor/workflows/child.wf
assert_exit 0
smokes_jq '.smokes | length == 2 and all(.status == "pass")'
assert_eq "workflow validate child --dry-run --json --path .
workflow validate parent --dry-run --json --path ." "$(cat "$STUB_DIR/calls")" "conductor calls"

# --- the real database (#3469) ------------------------------------------------------
# smoke_home HOME PATH... — like smoke, with CONDUCTOR_HOME set to HOME.
smoke_home() {
  local h="$1"
  shift
  STDOUT=$(printf '%s\n' "$@" | CONDUCTOR_HOME="$h" SMOKE_CONDUCTOR="$STUB_DIR/conductor" bash "$SCRIPTS/smoke-workflows.sh" 2>/dev/null)
  EXIT=$?
}
# with_schema N — commit a migrations.rs declaring LATEST_SCHEMA_VERSION = N to $repo.
with_schema() {
  commit_file "$repo" conductor-core/src/db/migrations.rs "pub const LATEST_SCHEMA_VERSION: u32 = $1;" schema
}

case_begin "same schema: smokes run on the real home with no db_note"
new_repo
stub_reset
with_schema 89
home=$(scratch_conductor_home 89)
smoke_home "$home" .conductor/workflows/child-x.wf
assert_exit 0
assert_eq "$home|unset" "$(cat "$STUB_DIR/env")" "conductor env"
smokes_jq '.smokes | length == 2 and all(.status == "pass") and all(has("db_note") | not)'

case_begin "branch adds a migration: smokes run on a copy, say so, and the real database is untouched"
new_repo
stub_reset
with_schema 90
home=$(scratch_conductor_home 89)
before=$(cksum < "$home/conductor.db")
smoke_home "$home" .conductor/workflows/child-x.wf
assert_exit 0
env_home=$(cut -d'|' -f1 < "$STUB_DIR/env")
[ "$env_home" != "$home" ] || fail "the smoke ran on the real home"
assert_eq "real-data" "$(cat "$STUB_DIR/marker" 2>/dev/null)" "copy holds the real data"
[ ! -e "$env_home" ] || fail "the copy was not removed: $env_home"
assert_eq "$before" "$(cksum < "$home/conductor.db")" "real database bytes"
smokes_jq '.smokes | length == 2 and all(.status == "pass") and all(.db_note | contains("89 to 90"))'

case_begin "real database newer than the branch: every smoke skipped with the reason, no call"
new_repo
stub_reset
with_schema 88
smoke_home "$(scratch_conductor_home 89)" .conductor/workflows/child-x.wf
assert_exit 0
smokes_jq '.smokes | length == 2 and all(.status == "skip") and all(.detail | contains("merge the base"))'
[ ! -e "$STUB_DIR/calls" ] || fail "conductor was called against a newer database"

case_begin "unreadable database: every smoke skipped, no call"
new_repo
stub_reset
with_schema 89
home=$(scratch_dir)
echo junk > "$home/conductor.db"
smoke_home "$home" .conductor/workflows/child.wf
assert_exit 0
smokes_jq '.smokes | length == 2 and all(.status == "skip") and all(.detail | contains("could not read"))'
[ ! -e "$STUB_DIR/calls" ] || fail "conductor was called against an unreadable database"

# --- update-pr-smokes.sh ------------------------------------------------------------
GH_DIR=$(scratch_dir)
export GH_STUB_DIR="$GH_DIR"
mkdir -p "$GH_DIR/bin"
cat > "$GH_DIR/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
echo "$*" >> "$d/calls"
case "$1 $2" in
  "pr view") cat "$d/body.md"; exit 0 ;;
  "pr edit")
    [ -f "$d/fail_edit" ] && { echo "boom" >&2; exit 1; }
    while [ "$#" -gt 0 ]; do
      [ "$1" = "--body-file" ] && { cp "$2" "$d/edited.md"; break; }
      shift
    done
    exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
chmod +x "$GH_DIR/bin/gh"

SMOKE_VERIFY=$(jq -nc '{step:"verify", iteration:0, context:"PASS", markers:[], structured_output: ({passed:true, checks:[], smokes:[{workflow:"child",reason:"changed",status:"pass",run_id:"01RUNNEW",detail:""}]} | tojson)}')
NOSMOKE_VERIFY=$(jq -nc '{step:"verify", iteration:0, context:"PASS", markers:[], structured_output: ({passed:true, checks:[], smokes:[]} | tojson)}')

# update BODY VERIFY_ENTRY [ENV=VAL...] — run update-pr-smokes.sh on a PR with that body.
update() {
  find "$GH_DIR" -maxdepth 1 -type f \( -name calls -o -name edited.md -o -name fail_edit \) -delete
  printf '%s\n' "$1" > "$GH_DIR/body.md"
  local entry="$2"
  shift 2
  run_script update-pr-smokes.sh PR_NUMBER=7 "PRIOR_CONTEXTS=[$entry]" "PATH=$GH_DIR/bin:$PATH" GH_RETRY_DELAYS="" "$@"
}

case_begin "update-pr-smokes: no smokes in verify means no gh call"
update "## Summary

s" "$NOSMOKE_VERIFY"
assert_exit 0
[ ! -e "$GH_DIR/calls" ] || fail "gh was called: $(cat "$GH_DIR/calls")"

case_begin "update-pr-smokes: an existing section is replaced, the rest intact"
update "## Summary

s

## Smokes

- \`child\` (changed): pass, run \`01RUNOLD\`

## Risks and open questions

- r" "$SMOKE_VERIFY"
assert_exit 0
edited=$(cat "$GH_DIR/edited.md" 2>/dev/null)
case "$edited" in *01RUNNEW*) ;; *) fail "new run ID missing: $edited" ;; esac
case "$edited" in *01RUNOLD*) fail "old section kept: $edited" ;; esac
case "$edited" in *"## Summary"*"## Smokes"*"## Risks and open questions"*"- r"*) ;; *) fail "sections out of order: $edited" ;; esac

case_begin "update-pr-smokes: with no section it is inserted before the Closes line"
update "## Summary

s

Closes #3453" "$SMOKE_VERIFY"
edited=$(cat "$GH_DIR/edited.md" 2>/dev/null)
case "$edited" in *"## Smokes"*"Closes #3453"*) ;; *) fail "expected Smokes before Closes: $edited" ;; esac

case_begin "update-pr-smokes: with neither it is appended"
update "## Summary

s" "$SMOKE_VERIFY"
edited=$(cat "$GH_DIR/edited.md" 2>/dev/null)
case "$edited" in *"## Summary"*"## Smokes"*01RUNNEW*) ;; *) fail "expected Smokes appended: $edited" ;; esac

case_begin "update-pr-smokes: a gh edit failure warns and exits 0"
update "## Summary

s" "$SMOKE_VERIFY"
touch "$GH_DIR/fail_edit"
run_script update-pr-smokes.sh PR_NUMBER=7 "PRIOR_CONTEXTS=[$SMOKE_VERIFY]" "PATH=$GH_DIR/bin:$PATH" GH_RETRY_DELAYS=""
assert_exit 0
assert_stderr_contains "could not update"

case_begin "update-pr-smokes: a pr-body.sh failure warns and is not reported as no smokes"
printf '%s\n' "## Summary" > "$GH_DIR/body.md"
rm -f "$GH_DIR/calls" "$GH_DIR/edited.md"
run_script update-pr-smokes.sh PR_NUMBER=7 "PRIOR_CONTEXTS=not json" "PATH=$GH_DIR/bin:$PATH" GH_RETRY_DELAYS=""
assert_exit 0
assert_stderr_contains "pr-body.sh failed"
assert_stdout_contains "Could not render smokes"
assert_stdout_not_contains "No workflow smokes"
[ ! -e "$GH_DIR/calls" ] || fail "gh was called: $(cat "$GH_DIR/calls")"

finish
