#!/usr/bin/env bash
# pr-body.sh: the PR description built from PRIOR_CONTEXTS (#3333).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

PLAN='{"summary":"Plan summary text.","risks":["A risk worth reading"],"tasks":[]}'
VERIFY_PASS='{"markers":[],"context":"PASS","passed":true,"checks":[{"name":"cargo nextest -p conductor-core","status":"pass","summary":"2433 tests run: 2433 passed, 0 skipped"},{"name":"cargo clippy","status":"pass","summary":"no warnings"}]}'

# entry STEP CONTEXT [STRUCTURED_OUTPUT_JSON] — one PRIOR_CONTEXTS element.
entry() {
  jq -nc --arg step "$1" --arg ctx "$2" --arg so "${3:-}" \
    '{step:$step, iteration:0, context:$ctx, markers:[]} + (if $so == "" then {} else {structured_output:$so} end)'
}

# contexts ENTRY... — a PRIOR_CONTEXTS array.
contexts() { printf '%s\n' "$@" | jq -sc .; }

body() { run_script pr-body.sh "PRIOR_CONTEXTS=$1"; }

case_begin "plan + implement + passing verify lists what verify ran"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Changed the builder.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_contains "## Summary"
assert_stdout_contains "Plan summary text."
assert_stdout_contains "## Implementation"
assert_stdout_contains "Changed the builder."
assert_stdout_contains "## Verification"
assert_stdout_contains "- cargo nextest -p conductor-core: 2433 tests run: 2433 passed, 0 skipped"
assert_stdout_contains "- cargo clippy: no warnings"

case_begin "the three real not-built/not-tested sentences are dropped, neighbours kept"
IMPL=$(printf '%s\n\n%s\n%s\n' \
  "Added the retry loop. Nothing was built or tested here, so the new tests are uncompiled. It also logs each attempt." \
  "Not run: cargo build/test/clippy/fmt (left to verify), and the script's FLOW_OUTPUT isn't exercised by a dry-run smoke." \
  "Reworded the brief. I didn't run any cargo commands, so none of it has been built or tested yet. Modified: a.sh")
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement "$IMPL")" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_not_contains "Nothing was built"
assert_stdout_not_contains "uncompiled"
assert_stdout_not_contains "Not run:"
assert_stdout_not_contains "left to verify"
assert_stdout_not_contains "didn't run any cargo"
assert_stdout_not_contains "tested yet"
assert_stdout_contains "Added the retry loop."
assert_stdout_contains "It also logs each attempt."
assert_stdout_contains "Reworded the brief."
assert_stdout_contains "Modified: a.sh"

case_begin "a sentence that mentions tests but not their status survives"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'The new test no longer runs the old code path. Tests were added for the parser.')")"
assert_exit 0
assert_stdout_contains "The new test no longer runs the old code path."
assert_stdout_contains "Tests were added for the parser."

case_begin "behaviour sentences with run/verified survive"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'The hook does not run on dry runs. It never run twice per push. The id is not verified by the caller.')")"
assert_exit 0
assert_stdout_contains "The hook does not run on dry runs."
assert_stdout_contains "It never run twice per push."
assert_stdout_contains "The id is not verified by the caller."

case_begin "sentences with a tooling word before 'not run' survive"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'The build step does not run on dry runs. Added tests so the hook never runs twice.')")"
assert_exit 0
assert_stdout_contains "The build step does not run on dry runs."
assert_stdout_contains "Added tests so the hook never runs twice."

case_begin "tooling not-run claims are still dropped"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Kept the loop. The tests were not run. Clippy has not been run. Done.')")"
assert_exit 0
assert_stdout_not_contains "tests were not run"
assert_stdout_not_contains "Clippy has not"
assert_stdout_contains "Kept the loop."

case_begin "contracted tooling not-run claims are dropped"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement "Kept the loop. The tests weren't run. Clippy hasn't been run. Done.")")"
assert_exit 0
assert_stdout_not_contains "weren't run"
assert_stdout_not_contains "Clippy hasn't"
assert_stdout_contains "Kept the loop."

case_begin "quick-fix-pr shape: no plan, implement-ticket summary becomes the Summary"
body "$(contexts "$(entry implement-ticket 'Fixed the crash. Nothing was built or tested here.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_contains "## Summary"
assert_stdout_contains "Fixed the crash."
assert_stdout_not_contains "Nothing was built"
assert_stdout_not_contains "## Implementation"
assert_stdout_contains "## Verification"

case_begin "verify with checks skipped uses verify's context"
SKIPPED='{"markers":[],"context":"PASS: no Rust or pipeline changes; checks skipped.","passed":true,"checks":[]}'
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Edited docs.')" "$(entry verify 'PASS: no Rust or pipeline changes; checks skipped.' "$SKIPPED")")"
assert_exit 0
assert_stdout_contains "## Verification"
assert_stdout_contains "PASS: no Rust or pipeline changes; checks skipped."

case_begin "after a verify loopback the last (passing) verify entry is used"
FAILING='{"markers":[],"context":"FAIL: clippy","passed":false,"checks":[{"name":"cargo clippy","status":"fail","summary":"3 warnings"}]}'
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify 'FAIL: clippy' "$FAILING")" "$(entry implement 'Fixed clippy.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_contains "- cargo nextest -p conductor-core"
assert_stdout_not_contains "3 warnings"
assert_stdout_not_contains "FAIL"

case_begin "no verify entry: no Verification section"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')")"
assert_exit 0
assert_stdout_contains "## Implementation"
assert_stdout_not_contains "## Verification"

case_begin "only verify: empty output so push-and-pr falls back to --fill"
body "$(contexts "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_eq "" "$STDOUT" "stdout"

case_begin "implement summary that is all not-built claims counts as empty"
body "$(contexts "$(entry implement 'Nothing was built or tested here.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_eq "" "$STDOUT" "stdout"

case_begin "unsubstituted template exits non-zero without stderr"
body '{{prior_contexts}}'
assert_exit_nonzero
assert_eq "" "$STDERR" "stderr"

case_begin "malformed JSON exits non-zero with jq's error"
body 'not json'
assert_exit_nonzero
[ -n "$STDERR" ] || fail "expected a parse error on stderr"

case_begin "risks follow the Verification section"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_contains "## Risks and open questions"
assert_stdout_contains "- A risk worth reading"
case "$STDOUT" in
  *"## Verification"*"## Risks and open questions"*) ;;
  *) fail "Verification should come before Risks" ;;
esac

finish
