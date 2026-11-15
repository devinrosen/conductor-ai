#!/usr/bin/env bash
# scripts/release/bump.sh against a scratch repo holding a dependency-free Cargo workspace.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

command -v cargo >/dev/null 2>&1 || die "cargo is required"

PLUGIN=plugin/.claude-plugin/plugin.json
BUMP="$REPO_ROOT/scripts/release/bump.sh"

# new_workspace — scratch repo at 0.1.0 with one commit; cd into it.
new_workspace() {
  repo=$(scratch_repo)
  cd "$repo" || die "cd failed"
  printf '[workspace]\nmembers = ["a"]\nresolver = "2"\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
  mkdir -p a/src "$(dirname "$PLUGIN")"
  printf '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n' > a/Cargo.toml
  : > a/src/lib.rs
  printf '{\n  "name": "scratch",\n  "version": "0.1.0"\n}\n' > "$PLUGIN"
  printf 'target\n' > .gitignore
  cargo generate-lockfile --offline >/dev/null 2>&1 || die "cargo generate-lockfile failed"
  printf 'other\n' > other.txt
  git add -A
  git commit -q -m init
}

run_bump() {
  local errf
  errf=$(mktemp "${TMPDIR:-/tmp}/pipeline-test-err.XXXXXX")
  STDOUT=$(bash "$BUMP" "$@" 2>"$errf")
  EXIT=$?
  STDERR=$(cat "$errf")
  rm -f "$errf"
}

EXPECTED_FILES=$(printf '%s\n' Cargo.lock Cargo.toml "$PLUGIN" | LC_ALL=C sort)

case_begin "commits only the three files as 'release: X.Y.Z' and prints the push command"
new_workspace
run_bump 0.2.0
assert_exit 0
assert_eq "release: 0.2.0" "$(git log -1 --format=%s)" "subject"
assert_eq "$EXPECTED_FILES" "$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)" "files"
assert_eq "" "$(git status --porcelain)" "tree status"
assert_stdout_contains "git push origin HEAD:release/0.2.0"

case_begin "--no-commit leaves HEAD alone and prints the commit command"
new_workspace
head=$(git rev-parse HEAD)
run_bump --no-commit 0.2.0
assert_exit 0
assert_eq "$head" "$(git rev-parse HEAD)" "HEAD"
assert_eq "$EXPECTED_FILES" "$(git diff --name-only | LC_ALL=C sort)" "modified files"
assert_eq "" "$(git diff --cached --name-only)" "staged files"
assert_stdout_contains 'git commit -m "release: 0.2.0"'

case_begin "--no-commit after the version works too"
new_workspace
head=$(git rev-parse HEAD)
run_bump 0.2.0 --no-commit
assert_exit 0
assert_eq "$head" "$(git rev-parse HEAD)" "HEAD"

case_begin "something staged: exit 1, names the file, nothing edited"
new_workspace
head=$(git rev-parse HEAD)
echo change >> other.txt
git add other.txt
run_bump 0.2.0
assert_exit 1
assert_stderr_contains "other.txt"
assert_eq "$head" "$(git rev-parse HEAD)" "HEAD"
assert_eq "" "$(git diff --name-only)" "unstaged edits"

case_begin "unstaged change to another file stays out of the commit"
new_workspace
echo change >> other.txt
run_bump 0.2.0
assert_exit 0
assert_eq "$EXPECTED_FILES" "$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)" "files"
assert_eq " M other.txt" "$(git status --porcelain)" "tree status"

case_begin "bad flag: exit 2"
new_workspace
run_bump --bogus 0.2.0
assert_exit 2

case_begin "missing version: exit 2"
new_workspace
run_bump --no-commit
assert_exit 2

case_begin "two versions: exit 2"
new_workspace
run_bump 0.2.0 0.3.0
assert_exit 2

finish
