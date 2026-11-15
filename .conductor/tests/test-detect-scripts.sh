#!/usr/bin/env bash
# detect-db-migrations.sh, detect-file-types.sh, detect-workflow-changes.sh,
# detect-engine-doc-changes.sh and lib-diff-range.sh against a scratch repo with a fake origin/main.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

repo=$(scratch_repo)
cd "$repo" || die "cd failed"
commit_file "$repo" README.md base base
git update-ref refs/remotes/origin/main HEAD
git checkout -q -b feature
commit_file "$repo" conductor-core/src/db/migrations/001_x.sql "select 1;" "A1"
commit_file "$repo" src/lib.rs "fn x() {}" "A2"
commit_file "$repo" .conductor/workflows/x.wf "workflow x" "A3"
A=$(git rev-parse HEAD)
commit_file "$repo" docs/x.md "docs" "B"
B=$(git rev-parse HEAD)
# A commit that is not an ancestor of HEAD.
git checkout -q -b side main
commit_file "$repo" side.txt side "side"
SIDE=$(git rev-parse HEAD)
git checkout -q feature

ALL="detect-db-migrations.sh detect-file-types.sh detect-workflow-changes.sh detect-engine-doc-changes.sh"

case_begin "full scope sets all three markers"
run_script detect-db-migrations.sh BASE_BRANCH=main
assert_exit 0; assert_marker has_db_migrations; assert_context_contains "the PR diff against origin/main"
run_script detect-file-types.sh BASE_BRANCH=main
assert_exit 0; assert_marker has_code_changes; assert_marker has_security_changes
run_script detect-workflow-changes.sh BASE_BRANCH=main
assert_exit 0; assert_marker has_workflow_changes

case_begin "SCOPE=full ignores SINCE"
run_script detect-db-migrations.sh BASE_BRANCH=main SCOPE=full "SINCE=$A"
assert_exit 0; assert_marker has_db_migrations

case_begin "incremental since A sees only the docs commit: no markers"
for s in $ALL; do
  run_script "$s" BASE_BRANCH=main SCOPE=incremental "SINCE=$A"
  assert_exit 0; assert_no_markers; assert_context_contains "commits since ${A:0:8}"
done

case_begin "incremental since a non-ancestor falls back to the full diff"
run_script detect-db-migrations.sh BASE_BRANCH=main SCOPE=incremental "SINCE=$SIDE"
assert_exit 0; assert_marker has_db_migrations
assert_context_contains "the PR diff against origin/main"
assert_stderr_contains "not an ancestor"

case_begin "incremental with an unsubstituted {{since}} falls back to the full diff"
run_script detect-workflow-changes.sh BASE_BRANCH=main SCOPE=incremental 'SINCE={{since}}'
assert_exit 0; assert_marker has_workflow_changes
assert_context_contains "the PR diff against origin/main"

case_begin "incremental with an empty SINCE falls back to the full diff"
run_script detect-file-types.sh BASE_BRANCH=main SCOPE=incremental SINCE=
assert_exit 0; assert_marker has_code_changes

case_begin "unset BASE_BRANCH fails"
for s in $ALL; do
  run_script "$s"
  assert_exit_nonzero; assert_stderr_contains "BASE_BRANCH"
done

case_begin "a missing base ref fails instead of reading as no files"
for s in $ALL; do
  run_script "$s" BASE_BRANCH=nope
  assert_exit_nonzero
  case "$STDOUT" in *FLOW_OUTPUT*) fail "$s emitted a FLOW_OUTPUT for a failed diff" ;; esac
done

# --- detect-file-types path filters, each in its own branch off origin/main ---
filter_case() { # NAME EXPECT(code|ci|no) FILE...
  local name="$1" expect="$2"
  shift 2
  git checkout -q -B filter origin/main
  local f
  for f in "$@"; do commit_file "$repo" "$f" x "add $f"; done
  run_script detect-file-types.sh BASE_BRANCH=main
  case_begin "detect-file-types: $name"
  assert_exit 0
  case "$expect" in
    code) assert_marker has_code_changes; assert_marker has_security_changes ;;
    ci) assert_marker has_security_changes; assert_no_marker has_code_changes ;;
    *) assert_no_markers ;;
  esac
}
filter_case ".conductor/scripts/*.sh counts as code" code .conductor/scripts/x.sh
filter_case ".conductor/tests/*.sh counts as code" code .conductor/tests/t.sh
filter_case "nested .conductor/tests .sh counts as code" code .conductor/tests/sub/t.sh
filter_case "scripts/release/*.sh counts as code" code scripts/release/x.sh
filter_case "a root *.sh counts as code" code install-cli.sh
filter_case ".githooks/ files count as code" code .githooks/pre-push
filter_case "docs/examples/hooks/*.sh is not code" no docs/examples/hooks/x.sh
filter_case ".conductor/tests fixtures are not code" no .conductor/tests/fixtures/x.json
filter_case "a non-.sh file under .conductor/tests is not code" no .conductor/tests/t.rs
filter_case "root README.md is not code" no README2.md
filter_case "docs/ is not code" no docs/y.rs
filter_case ".github/workflows/*.yml gets a security review" ci .github/workflows/ci.yml
filter_case ".github/workflows/*.yaml gets a security review" ci .github/workflows/ci.yaml
filter_case ".github/actions/*/action.yml gets a security review" ci .github/actions/setup/action.yml
filter_case "other .github/ files get no review" no .github/x.js .github/ISSUE_TEMPLATE/bug.md .github/dependabot.yml
filter_case ".conductor/workflows is not code" no .conductor/workflows/y.rs
filter_case "nested .md is not a code extension" no pkg/notes.md
filter_case "Cargo.lock is code" code Cargo.lock
filter_case "code plus a workflow sets both markers" code src/lib.rs .github/workflows/ci.yml
filter_case "a file name with a space and quote yields valid JSON" code "src/we ird \"name\".rs"

# --- detect-engine-doc-changes (#3336) ---
# A stub cargo that only logs, and a fake CARGO_HOME, so the tests never reach
# the network or the real registry.
DOC_STUB=$(scratch_dir)
mkdir -p "$DOC_STUB/bin"
printf '#!/usr/bin/env bash\necho "$*" >> "%s/calls"\nexit 0\n' "$DOC_STUB" > "$DOC_STUB/bin/cargo"
chmod +x "$DOC_STUB/bin/cargo"
FAKE_HOME=$(scratch_dir)
mkdir -p "$FAKE_HOME/registry/src/idx/runkon-flow-1.2.3-alpha"
LOCK_BODY=$(printf '[[package]]\nname = "runkon-flow-executors"\nversion = "9.9.9"\n\n[[package]]\nname = "runkon-flow"\nversion = "1.2.3-alpha"\n')

doc_case() { # NAME EXPECT(yes|no) FILE...
  local name="$1" expect="$2"
  shift 2
  git checkout -q -B docs origin/main
  commit_file "$repo" Cargo.lock "$LOCK_BODY" "lock"
  local f
  for f in "$@"; do commit_file "$repo" "$f" x "add $f"; done
  rm -f "$DOC_STUB/calls"
  run_script detect-engine-doc-changes.sh BASE_BRANCH=main "CARGO_HOME=$FAKE_HOME" "PATH=$DOC_STUB/bin:$PATH"
  case_begin "detect-engine-doc-changes: $name"
  assert_exit 0
  if [ "$expect" = yes ]; then
    assert_marker has_engine_doc_changes
    assert_jq '.runkon_version == "1.2.3-alpha"'
    assert_jq ".runkon_src == \"$FAKE_HOME/registry/src/idx\""
  else
    assert_no_markers
    assert_jq '.runkon_version == "" and .runkon_src == ""'
  fi
}
doc_case "docs/workflow/language.md triggers" yes docs/workflow/language.md
doc_case "a nested docs/workflow file triggers" yes docs/workflow/sub/x.md
doc_case "the Workflow Semantics brief triggers" yes .conductor/agents/review-workflow-semantics.md
doc_case "another agent brief does not" no .conductor/agents/review-security.md
doc_case "docs/VISION.md does not" no docs/VISION.md
doc_case "README.md does not" no README.md

case_begin "detect-engine-doc-changes: the locked version is used, not another package's"
git checkout -q -B docs origin/main
commit_file "$repo" Cargo.lock "$LOCK_BODY" "lock"
commit_file "$repo" docs/workflow/language.md x "doc"
run_script detect-engine-doc-changes.sh BASE_BRANCH=main "CARGO_HOME=$FAKE_HOME" "PATH=$DOC_STUB/bin:$PATH"
assert_jq '.runkon_version == "1.2.3-alpha"'
[ ! -f "$DOC_STUB/calls" ] || fail "cargo ran although the source was present"

case_begin "detect-engine-doc-changes: a missing source still emits the marker, after cargo fetch"
EMPTY_HOME=$(scratch_dir)
mkdir -p "$EMPTY_HOME/registry/src/idx"
rm -f "$DOC_STUB/calls"
run_script detect-engine-doc-changes.sh BASE_BRANCH=main "CARGO_HOME=$EMPTY_HOME" "PATH=$DOC_STUB/bin:$PATH"
assert_exit 0; assert_marker has_engine_doc_changes
assert_jq '.runkon_version == "1.2.3-alpha" and .runkon_src == ""'
assert_stderr_contains "not found"
grep -q '^fetch' "$DOC_STUB/calls" 2>/dev/null || fail "cargo fetch was not called"

case_begin "detect-engine-doc-changes: no Cargo.lock gives an empty version and still exits 0"
git checkout -q -B docs origin/main
commit_file "$repo" docs/workflow/language.md x "doc"
run_script detect-engine-doc-changes.sh BASE_BRANCH=main "CARGO_HOME=$FAKE_HOME" "PATH=$DOC_STUB/bin:$PATH"
assert_exit 0; assert_marker has_engine_doc_changes
assert_jq '.runkon_version == "" and .runkon_src == ""'

finish
