#!/usr/bin/env bash
# show-plan-decisions.sh: lists the plan's open decisions; never fails the step.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

repo=$(scratch_repo)
cd "$repo" || die "cd failed"

run() { run_script show-plan-decisions.sh "PRIOR_OUTPUT=$1"; }

case_begin "valid decisions are listed with their defaults"
run '{"decisions":[{"question":"Use bats?","default":"plain bash"},{"question":"Port now?","default":"no"}]}'
assert_exit 0
assert_no_markers
assert_context_contains "1. Use bats?"
assert_context_contains "Default: plain bash"
assert_context_contains "2. Port now?"
assert_context_contains "Default: no"

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
assert_context_contains "PLAN.md"
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

finish
