#!/usr/bin/env bash
# fetch-plan-decisions.sh against a stub gh: which plan-gate decisions reach the reviewers (#3484).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# The stub gh, driven by files in $GH_STUB_DIR:
#   pr.json    the PR object `gh api repos/o/r/pulls/7` prints
#   fail_api   when present, that call fails
#   calls      every gh call is appended here
# `gh pr view` always fails (no PR for the branch). Any other call fails loudly.
write_stub() { # DIR
  mkdir -p "$1/bin"
  cat > "$1/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
echo "$*" >> "$d/calls"
case "$1 $2" in
  "repo view") echo "o/r"; exit 0 ;;
  "pr view") exit 1 ;;
  "api repos/o/r/pulls/7")
    [ -e "$d/fail_api" ] && exit 1
    cat "$d/pr.json"; exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
  chmod +x "$1/bin/gh"
}

BOT='conductor-ai-reviewer[bot]'
SECTION=$'The plan gate settled these before implementation:\n\n1. Take the breaking change?\n   - Default: No\n\n2. Rename the flag?\n   - Default: Keep the name\n\nAnswer at the gate: kept the defaults.'

# pr_json ASSOC BODY
pr_json() {
  jq -nc --arg assoc "$1" --arg body "$2" '{author_association: $assoc, user: {login: "someone", type: "User"}, body: $body}'
}

new_case() { # PR_JSON
  stub=$(scratch_dir)
  write_stub "$stub"
  printf '%s\n' "$1" > "$stub/pr.json"
}

run_fetch() { # [ENV=VAL...]
  run_script fetch-plan-decisions.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" TRUSTED_REVIEW_BOTS="$BOT" "$@"
}

case_begin "the input is used and gh is not called"
new_case "$(pr_json OWNER "")"
run_fetch PR_NUMBER=7 PLAN_DECISIONS="$SECTION"
assert_exit 0
assert_no_markers
assert_jq '.decisions_source == "input" and .decision_count == 2'
assert_jq '.user_decisions | contains("1. Take the breaking change?")'
[ ! -e "$stub/calls" ] || fail "gh was called: $(cat "$stub/calls")"

case_begin "a literal template input reads the PR body's section, up to the next heading"
BODY=$(printf '## Summary\n\nS.\n\n## Decisions\n\n%s\n\n## Verification\n\n1. not a decision\n' "$SECTION")
new_case "$(pr_json MEMBER "$BODY")"
run_fetch PR_NUMBER=7 PLAN_DECISIONS='{{plan_decisions}}'
assert_exit 0
assert_jq '.decisions_source == "pr_body" and .decision_count == 2'
assert_jq '.user_decisions | contains("2. Rename the flag?") and (contains("not a decision") | not) and (contains("## ") | not)'

case_begin "an empty input also falls back to the PR body"
run_fetch PR_NUMBER=7 PLAN_DECISIONS=
assert_jq '.decisions_source == "pr_body" and .decision_count == 2'

case_begin "an untrusted author gives None"
new_case "$(pr_json NONE "$BODY")"
run_fetch PR_NUMBER=7 PLAN_DECISIONS=
assert_exit 0
assert_jq '.decisions_source == "none" and .decision_count == 0 and (.user_decisions | startswith("None"))'

case_begin "a body with no Decisions section gives None, count 0"
new_case "$(pr_json OWNER $'## Summary\n\nS.\n')"
run_fetch PR_NUMBER=7 PLAN_DECISIONS=
assert_exit 0
assert_jq '.decisions_source == "none" and .decision_count == 0 and (.user_decisions | startswith("None"))'

case_begin "no PR number gives None"
new_case "$(pr_json OWNER "$BODY")"
run_fetch PR_NUMBER= PLAN_DECISIONS=
assert_exit 0
assert_jq '.decisions_source == "none" and .decision_count == 0'

case_begin "a gh failure gives None and exits 0"
new_case "$(pr_json OWNER "$BODY")"
touch "$stub/fail_api"
run_fetch PR_NUMBER=7 PLAN_DECISIONS=
assert_exit 0
assert_jq '.decisions_source == "none" and .decision_count == 0'
assert_stderr_contains "could not fetch PR #7"

case_begin "over-long text is truncated"
LONG="1. $(head -c 6000 /dev/zero | tr '\0' x)"
new_case "$(pr_json OWNER "")"
run_fetch PR_NUMBER=7 PLAN_DECISIONS="$LONG"
assert_exit 0
assert_jq '(.user_decisions | endswith("[truncated]")) and (.user_decisions | length) < 4100 and .decision_count == 1'

finish
