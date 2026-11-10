#!/usr/bin/env bash
# detect-db-migrations.sh, detect-file-types.sh, detect-workflow-changes.sh and
# lib-diff-range.sh against a scratch repo with a fake origin/main.
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

ALL="detect-db-migrations.sh detect-file-types.sh detect-workflow-changes.sh"

case_begin "full scope sets all three markers"
run_script detect-db-migrations.sh BASE_BRANCH=main
assert_exit 0; assert_marker has_db_migrations; assert_context_contains "the PR diff against origin/main"
run_script detect-file-types.sh BASE_BRANCH=main
assert_exit 0; assert_marker has_code_changes
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
filter_case() { # NAME EXPECT(yes|no) FILE...
  local name="$1" expect="$2"
  shift 2
  git checkout -q -B filter origin/main
  local f
  for f in "$@"; do commit_file "$repo" "$f" x "add $f"; done
  run_script detect-file-types.sh BASE_BRANCH=main
  case_begin "detect-file-types: $name"
  assert_exit 0
  if [ "$expect" = yes ]; then assert_marker has_code_changes; else assert_no_markers; fi
}
filter_case ".conductor/scripts/*.sh counts as code" yes .conductor/scripts/x.sh
filter_case "root README.md is not code" no README2.md
filter_case "docs/ is not code" no docs/y.rs
filter_case ".github/ is not code" no .github/x.js
filter_case ".conductor/workflows is not code" no .conductor/workflows/y.rs
filter_case ".conductor/tests is not code" no .conductor/tests/t.rs
filter_case "nested .md is not a code extension" no pkg/notes.md
filter_case "Cargo.lock is code" yes Cargo.lock
filter_case "a file name with a space and quote yields valid JSON" yes "src/we ird \"name\".rs"

finish
