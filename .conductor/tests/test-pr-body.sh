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

body() { run_script pr-body.sh "PRIOR_CONTEXTS=$1" "${@:2}"; }

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

case_begin "sentences naming a local plan path are dropped from the summary and implementation"
PLAN_PATHS='{"summary":"Adds X. Plan written to /Users/me/r/.git/worktrees/w/PLAN.md.","tasks":[]}'
body "$(contexts "$(entry plan x "$PLAN_PATHS")" "$(entry implement 'Changed Y. Plan at /home/me/r/.git/worktrees/w/PLAN.md. See ~/.conductor/config.toml.')")"
assert_exit 0
assert_stdout_contains "Adds X."
assert_stdout_contains "Changed Y."
assert_stdout_contains "~/.conductor/config.toml"
case "$STDOUT" in *"/Users/"*|*"/home/"*|*PLAN.md*) fail "local path left in the body" ;; esac

case_begin "a plan summary that is only the plan path falls back to the implement summary"
PLAN_ONLY='{"summary":"Plan: /Users/me/r/.git/worktrees/w/PLAN.md","tasks":[]}'
body "$(contexts "$(entry plan x "$PLAN_ONLY")" "$(entry implement 'Changed Y.')")"
assert_exit 0
assert_stdout_contains "## Summary"
assert_stdout_contains "Changed Y."
case "$STDOUT" in *PLAN.md*|*"## Implementation"*) fail "path kept or no fallback" ;; esac

case_begin "unsubstituted template exits non-zero without stderr"
body '{{prior_contexts}}'
assert_exit_nonzero
assert_eq "" "$STDERR" "stderr"

case_begin "step outputs that mention {{var}} still build the full description"
BRACE_PLAN='{"summary":"Plan mentions {{prior_contexts}} text.","risks":["Risk about {{var}} syntax"],"tasks":[]}'
body "$(contexts "$(entry plan x "$BRACE_PLAN")" "$(entry implement 'Used {{ticket_id}} in the prompt.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_exit 0
assert_stdout_contains "## Summary"
assert_stdout_contains "Plan mentions {{prior_contexts}} text."
assert_stdout_contains "## Implementation"
assert_stdout_contains "Used {{ticket_id}} in the prompt."
assert_stdout_contains "## Verification"
assert_stdout_contains "## Risks and open questions"
assert_stdout_contains "- Risk about {{var}} syntax"

case_begin "unsubstituted template with surrounding whitespace exits non-zero without stderr"
body $'  {{prior_contexts}}\n'
assert_exit_nonzero
assert_eq "" "$STDERR" "stderr"

case_begin "empty PRIOR_CONTEXTS exits non-zero without stderr"
body ''
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

DPLAN='{"summary":"Plan summary text.","risks":["A risk worth reading"],"tasks":[],"decisions":[{"question":"Fix runkon warnings here?","default":"No, leave them","defers":"prints no failure warnings -> runkon/runkon#209"},{"question":"Rename the flag?","default":"Keep the name"}]}'
dbody() { body "$(contexts "$(entry plan x "$DPLAN")" "$(entry implement 'Did it.')" "$(entry verify PASS "$VERIFY_PASS")")" "$@"; }

case_begin "decisions + empty feedback: the section lists defaults, Defers and the deferred items"
dbody "PLAN_GATE_FEEDBACK="
assert_exit 0
assert_stdout_contains "## Decisions"
assert_stdout_contains "The plan gate settled these before implementation:"
assert_stdout_contains "1. Fix runkon warnings here?"
assert_stdout_contains "   - Default: No, leave them"
assert_stdout_contains "   - Defers: prints no failure warnings -> runkon/runkon#209"
assert_stdout_contains "Answer at the gate: kept the defaults."
assert_stdout_contains "Deferred acceptance items: prints no failure warnings -> runkon/runkon#209"
assert_stdout_contains "2. Rename the flag?"

case_begin "decisions + multi-line feedback: the answer is quoted and no deferred-items line"
dbody "PLAN_GATE_FEEDBACK=Fix them here.
Keep {braces} as is."
assert_exit 0
assert_stdout_contains "> Fix them here."
assert_stdout_contains "> Keep {braces} as is."
assert_stdout_contains "Defers (if the default was kept): prints no failure warnings -> runkon/runkon#209"
assert_stdout_not_contains "Deferred acceptance items:"
assert_stdout_not_contains "kept the defaults"

case_begin "the gate never ran (literal template or unset): no Decisions section"
dbody "PLAN_GATE_FEEDBACK={{human_review.feedback}}"
assert_exit 0
assert_stdout_not_contains "## Decisions"
dbody
assert_exit 0
assert_stdout_not_contains "## Decisions"

case_begin "no decisions in the plan: no section, even with feedback"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')")" "PLAN_GATE_FEEDBACK=go"
assert_exit 0
assert_stdout_not_contains "## Decisions"

case_begin "decisions: a local path in a default is scrubbed"
LPLAN='{"summary":"S.","decisions":[{"question":"Q?","default":"See /Users/me/x/PLAN.md. Keep it."}]}'
body "$(contexts "$(entry plan x "$LPLAN")" "$(entry implement 'Did it.')")" "PLAN_GATE_FEEDBACK="
assert_exit 0
assert_stdout_contains "## Decisions"
assert_stdout_not_contains "/Users/me"
assert_stdout_contains "Keep it."

case_begin "decisions sit between Implementation and Verification"
dbody "PLAN_GATE_FEEDBACK="
case "$STDOUT" in
  *"## Summary"*"## Implementation"*"## Decisions"*"## Verification"*"## Risks and open questions"*) ;;
  *) fail "section order should be Summary, Implementation, Decisions, Verification, Risks" ;;
esac

SMOKES='{"markers":[],"context":"PASS","passed":true,"checks":[{"name":"cargo build","status":"pass","summary":"ok"}],"smokes":[{"workflow":"child","reason":"changed","status":"pass","run_id":"01RUNCHILD","detail":""},{"workflow":"parent","reason":"calls child","status":"pass","run_id":"01RUNPARENT","detail":""},{"workflow":"ticket-to-pr","reason":"calls parent","status":"skip","run_id":"","detail":"no value for required input ticket_id"}]}'

case_begin "smokes: a Smokes section after Verification with run IDs and the skip named"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify PASS "$SMOKES")")"
assert_exit 0
assert_stdout_contains "## Smokes"
assert_stdout_contains '- `child` (changed): pass, run `01RUNCHILD`'
assert_stdout_contains '- `parent` (calls child): pass, run `01RUNPARENT`'
assert_stdout_contains '- `ticket-to-pr` (calls parent): skipped, no value for required input ticket_id'
case "$STDOUT" in
  *"## Verification"*"## Smokes"*"## Risks and open questions"*) ;;
  *) fail "section order should be Verification, Smokes, Risks" ;;
esac

case_begin "smokes: a failing smoke is listed as FAIL"
FAILSM='{"passed":false,"checks":[],"smokes":[{"workflow":"child","reason":"changed","status":"fail","run_id":"01RUNBAD","detail":"x"}]}'
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify FAIL "$FAILSM")")"
assert_stdout_contains '- `child` (changed): FAIL, run `01RUNBAD`'

case_begin "smokes: none or an empty list gives no section"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify PASS "$VERIFY_PASS")")"
assert_stdout_not_contains "## Smokes"
EMPTYSM=$(printf '%s' "$VERIFY_PASS" | jq -c '. + {smokes: []}')
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')" "$(entry verify PASS "$EMPTYSM")")"
assert_stdout_not_contains "## Smokes"

case_begin "smokes: PR_BODY_SECTION=smokes prints only the section"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry verify PASS "$SMOKES")")" PR_BODY_SECTION=smokes
assert_stdout_contains "## Smokes"
assert_stdout_not_contains "## Summary"
body "$(contexts "$(entry verify PASS "$VERIFY_PASS")")" PR_BODY_SECTION=smokes
assert_eq "" "$STDOUT" "section-only output without smokes"

case_begin "smokes: a run against a database copy says so under the list (#3469)"
COPYSM=$(printf '%s' "$SMOKES" | jq -c '.smokes |= map(. + {db_note: "a copy of the conductor database: this branch migrates it from schema 89 to 90"})')
body "$(contexts "$(entry plan x "$PLAN")" "$(entry verify PASS "$COPYSM")")" PR_BODY_SECTION=smokes
assert_stdout_contains '- `child` (changed): pass, run `01RUNCHILD`'
assert_stdout_contains "Run against a copy of the conductor database: this branch migrates it from schema 89 to 90."
body "$(contexts "$(entry plan x "$PLAN")" "$(entry verify PASS "$SMOKES")")" PR_BODY_SECTION=smokes
assert_stdout_not_contains "Run against"

case_begin "decisions mode: prints the section body without the heading (#3484)"
dbody "PR_BODY_SECTION=decisions" "PLAN_GATE_FEEDBACK=Fix them here."
assert_exit 0
assert_stdout_contains "The plan gate settled these before implementation:"
assert_stdout_contains "1. Fix runkon warnings here?"
assert_stdout_contains "> Fix them here."
assert_stdout_not_contains "## Decisions"
assert_stdout_not_contains "## Summary"

case_begin "decisions mode: the gate never ran prints nothing"
dbody "PR_BODY_SECTION=decisions" "PLAN_GATE_FEEDBACK={{human_review.feedback}}"
assert_exit 0
assert_eq "" "$STDOUT" "decisions output when the gate never ran"

case_begin "decisions mode: no decisions in the plan prints nothing"
body "$(contexts "$(entry plan x "$PLAN")" "$(entry implement 'Did it.')")" PR_BODY_SECTION=decisions "PLAN_GATE_FEEDBACK=go"
assert_exit 0
assert_eq "" "$STDOUT" "decisions output without decisions"

finish
