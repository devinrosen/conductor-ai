#!/usr/bin/env bash
# lib.sh — helpers for the pipeline script tests. Source it from a test-*.sh.
# bash 3.2 compatible: no mapfile, no associative arrays, no ${var,,}.

: "${TESTS_DIR:=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
: "${REPO_ROOT:=$(cd "$TESTS_DIR/../.." && pwd)}"
: "${SCRIPTS:=$REPO_ROOT/.conductor/scripts}"

# Isolate git from the developer's config (signing, hooks, templates,
# init.defaultBranch), the same lesson as conductor_cmd() (#3235).
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com

CASE_NAME=""
CASE_FAILED=0
FAILED_CASES=0
SCRATCH_DIRS=""
STDOUT=""
STDERR=""
EXIT=0

_cleanup() {
  local d
  for d in $SCRATCH_DIRS; do rm -rf "$d"; done
}
trap _cleanup EXIT

# Print and exit the file with a failure; used when the environment is broken.
die() {
  echo "FAIL  ${CASE_NAME:-setup}: $*"
  exit 1
}

case_begin() {
  case_end
  CASE_NAME="$1"
  CASE_FAILED=0
}

case_end() {
  if [ -n "$CASE_NAME" ]; then
    if [ "$CASE_FAILED" -eq 0 ]; then
      echo "PASS  $CASE_NAME"
    else
      FAILED_CASES=$((FAILED_CASES + 1))
    fi
  fi
  CASE_NAME=""
}

# Call at the end of a test file; exits non-zero when any case failed.
finish() {
  case_end
  [ "$FAILED_CASES" -eq 0 ]
  exit $?
}

fail() {
  if [ "$CASE_FAILED" -eq 0 ]; then
    echo "FAIL  $CASE_NAME"
  fi
  CASE_FAILED=1
  echo "      $*"
}

# scratch_dir — print a new temp dir, removed when the test file exits.
scratch_dir() {
  local d
  d=$(mktemp -d "${TMPDIR:-/tmp}/pipeline-test.XXXXXX") || die "mktemp failed"
  SCRATCH_DIRS="$SCRATCH_DIRS $d"
  echo "$d"
}

# scratch_repo — create a git repo on branch main with one commit; print its path.
scratch_repo() {
  local d
  d=$(scratch_dir)
  git -C "$d" init -q >/dev/null 2>&1 || die "git init failed"
  git -C "$d" symbolic-ref HEAD refs/heads/main
  git -C "$d" config commit.gpgsign false
  git -C "$d" config core.hooksPath /dev/null
  echo "$d"
}

# commit_file REPO PATH CONTENT MSG — write a file and commit it.
commit_file() {
  mkdir -p "$(dirname "$1/$2")"
  printf '%s\n' "$3" > "$1/$2"
  git -C "$1" add -A
  git -C "$1" commit -q -m "$4"
}

# run_script NAME [ENV=VAL...] — run $SCRIPTS/NAME in the current directory with
# the given extra environment; set STDOUT, STDERR and EXIT.
run_script() {
  local name="$1"
  shift
  local errf
  errf=$(mktemp "${TMPDIR:-/tmp}/pipeline-test-err.XXXXXX")
  STDOUT=$(env "$@" bash "$SCRIPTS/$name" 2>"$errf")
  EXIT=$?
  STDERR=$(cat "$errf")
  rm -f "$errf"
}

# flow_output — print the JSON between the FLOW_OUTPUT markers of $STDOUT.
flow_output() {
  printf '%s\n' "$STDOUT" | awk '
    /^<<<END_FLOW_OUTPUT>>>/ { p = 0 }
    p { print }
    /^<<<FLOW_OUTPUT>>>/ { p = 1 }'
}

# require_flow_output — fail the case unless the FLOW_OUTPUT block is valid JSON.
require_flow_output() {
  local out
  out=$(flow_output)
  if [ -z "$out" ] || ! printf '%s' "$out" | jq -e 'type == "object"' >/dev/null 2>&1; then
    fail "FLOW_OUTPUT block missing or not a JSON object; stdout: $STDOUT"
    return 1
  fi
}

assert_eq() { # EXPECTED ACTUAL [WHAT]
  if [ "$1" != "$2" ]; then
    fail "${3:-value}: expected [$1], got [$2]"
  fi
}

assert_exit() {
  if [ "$EXIT" -ne "$1" ]; then
    fail "exit code: expected $1, got $EXIT; stderr: $STDERR"
  fi
}

assert_exit_nonzero() {
  if [ "$EXIT" -eq 0 ]; then
    fail "expected a non-zero exit, got 0; stdout: $STDOUT"
  fi
}

# assert_jq EXPR — jq -e EXPR over the FLOW_OUTPUT must be truthy.
assert_jq() {
  require_flow_output || return 0
  if ! flow_output | jq -e "$1" >/dev/null 2>&1; then
    fail "jq check failed: $1"
    echo "      flow output: $(flow_output | jq -c .)"
  fi
}

assert_marker() {
  assert_jq "(.markers | index(\"$1\")) != null"
}

assert_no_marker() {
  assert_jq "(.markers | index(\"$1\")) == null"
}

assert_no_markers() {
  assert_jq '.markers == []'
}

assert_stderr_contains() {
  case "$STDERR" in
    *"$1"*) ;;
    *) fail "stderr does not contain [$1]; stderr: $STDERR" ;;
  esac
}

assert_context_contains() {
  require_flow_output || return 0
  local ctx
  ctx=$(flow_output | jq -r '.context')
  case "$ctx" in
    *"$1"*) ;;
    *) fail "context does not contain [$1]; context: $ctx" ;;
  esac
}
