#!/usr/bin/env bash
# show-plan-decisions.sh: lists the plan's open decisions; never fails the step.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

repo=$(scratch_repo)
cd "$repo" || die "cd failed"

expected="$(git -C "$repo" rev-parse --absolute-git-dir)/PLAN.md"

run() { run_script show-plan-decisions.sh "PRIOR_OUTPUT=$1"; }

case_begin "valid decisions are listed with their defaults"
run '{"decisions":[{"question":"Use bats?","default":"plain bash"},{"question":"Port now?","default":"no"}]}'
assert_exit 0
assert_no_markers
assert_context_contains "1. Use bats?"
assert_context_contains "Default: plain bash"
assert_context_contains "2. Port now?"
assert_context_contains "Default: no"
assert_context_contains "$expected"
assert_jq ".plan_path == \"$expected\""

case_begin "missing question and default get placeholders"
run '{"decisions":[{}]}'
assert_exit 0
assert_context_contains "(no question)"
assert_context_contains "(none given)"

case_begin "empty decisions list points at PLAN.md"
run '{"decisions":[]}'
assert_exit 0
assert_no_markers
assert_context_contains "listed none"
assert_context_contains "$expected"
assert_jq ".plan_path == \"$expected\""
assert_stderr_contains "decisions list is empty"

case_begin "missing decisions key counts as empty"
run '{}'
assert_exit 0
assert_context_contains "listed none"

case_begin "empty PRIOR_OUTPUT is unreadable but exits 0"
run ''
assert_exit 0
assert_no_markers
assert_context_contains "couldn't be read"
assert_context_contains "$expected"
assert_stderr_contains "isn't a JSON object"

case_begin "invalid JSON is unreadable but exits 0"
run 'not json'
assert_exit 0
assert_no_markers
assert_context_contains "couldn't be read"

case_begin "a JSON array is unreadable but exits 0"
run '[{"question":"x"}]'
assert_exit 0
assert_no_markers
assert_context_contains "couldn't be read"

case_begin "a linked worktree gives the worktree's git dir"
commit_file "$repo" a.txt a init
wt="$(scratch_dir)/linked"
git -C "$repo" worktree add -q "$wt" -b linked >/dev/null 2>&1 || die "git worktree add failed"
cd "$wt" || die "cd failed"
run '{"decisions":[{"question":"q","default":"d"}]}'
cd "$repo" || die "cd failed"
assert_exit 0
wt_expected="$(git -C "$wt" rev-parse --absolute-git-dir)/PLAN.md"
assert_context_contains "$wt_expected"
assert_jq ".plan_path == \"$wt_expected\""
assert_jq '.plan_path | startswith("/") and contains("/.git/worktrees/linked/PLAN.md")'

case_begin "outside a git repo the step still succeeds"
nogit=$(scratch_dir)
cd "$nogit" || die "cd failed"
GIT_CEILING_DIRECTORIES="$(dirname "$nogit")" run '{"decisions":[{"question":"q","default":"d"}]}'
cd "$repo" || die "cd failed"
assert_exit 0
assert_no_markers
assert_context_contains "1. q"
assert_context_contains "git rev-parse --absolute-git-dir"
assert_jq '.plan_path == ""'
assert_stderr_contains "not in a git repository"

finish
