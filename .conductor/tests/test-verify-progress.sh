#!/usr/bin/env bash
# verify-progress.sh: compares the last two verify signatures (#3486).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# v CONTEXT COUNT FAILURES_JSON — a verify entry whose structured_output is a JSON string.
v() {
  jq -cn --arg ctx "$1" --argjson n "$2" --argjson f "$3" \
    '{step: "verify", iteration: 0, context: $ctx, markers: ["has_failures"],
      structured_output: ({markers: ["has_failures"], context: $ctx, failures: $f, failure_count: $n} | tojson)}'
}
other() { jq -cn --arg s "$1" '{step: $s, iteration: 0, context: "x", markers: [], structured_output: null}'; }
arr() { jq -cs '.' ; }

FOUR='["cargo build: e1","cargo build: e2","cargo build: e3","cargo build: e4"]'
ONE='["cargo nextest -p c: t1"]'

case_begin "four compile errors then one test shrank"
pc=$({ v "FAIL: build" 4 "$FOUR"; v "FAIL: test t1" 1 "$ONE"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_exit 0
assert_marker failures_shrank
assert_context_contains "Failures shrank: 4 → 1"
assert_context_contains "FAIL: test t1"

case_begin "identical signatures stop"
pc=$({ v "a" 4 "$FOUR"; v "b" 4 "$FOUR"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_exit 0
assert_no_markers
assert_context_contains "stopping"
assert_context_contains "b"

case_begin "different signature, same count stops"
pc=$({ v "a" 1 '["x"]'; v "b" 1 '["y"]'; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_no_markers

case_begin "count grew stops"
pc=$({ v "a" 1 "$ONE"; v "b" 4 "$FOUR"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_no_markers

case_begin "a single verify stops"
pc=$({ v "a" 4 "$FOUR"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_exit 0
assert_no_markers

case_begin "a verify without a signature stops"
pc=$({ jq -cn '{step: "verify", context: "old", structured_output: ({markers: [], context: "old"} | tojson)}'; v "b" 1 "$ONE"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_exit 0
assert_no_markers

case_begin "null or malformed structured_output stops"
pc=$({ jq -cn '{step: "verify", context: "a", structured_output: null}'; jq -cn '{step: "verify", context: "b", structured_output: "{not json"}'; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_exit 0
assert_no_markers

case_begin "empty, malformed or non-array PRIOR_CONTEXTS stops"
for input in "" "not json" "null" '{"step":"verify"}'; do
  run_script verify-progress.sh PRIOR_CONTEXTS="$input"
  assert_exit 0
  assert_no_markers
done

case_begin "three verifies compare only the last two"
pc=$({ v "a" 9 '["a1"]'; v "b" 4 "$FOUR"; v "c" 1 "$ONE"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_marker failures_shrank
assert_context_contains "4 → 1"
pc=$({ v "a" 1 "$ONE"; v "b" 4 "$FOUR"; v "c" 4 "$FOUR"; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_no_markers

case_begin "other steps between and after the verifies are ignored"
pc=$({ other plan; v "a" 4 "$FOUR"; other implement; other fmt-fix-pre-reverify; v "b" 1 "$ONE"; other fix-precheck; } | arr)
run_script verify-progress.sh PRIOR_CONTEXTS="$pc"
assert_marker failures_shrank
assert_context_contains "b"

finish
