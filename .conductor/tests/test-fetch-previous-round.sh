#!/usr/bin/env bash
# fetch-previous-round.sh against a stub gh: which review's findings reach the reviewers (#3465).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# The stub gh, driven by files in $GH_STUB_DIR:
#   reviews.json  one JSON array: the PR's reviews (`gh api --paginate .../reviews`, which the
#                 script reads with `--jq '.[]'`, so the stub prints one review per line)
#   fail_reviews  when present, that call fails
# `gh pr view` always fails (no PR for the branch). Any other call fails loudly.
write_stub() { # DIR
  mkdir -p "$1/bin"
  cat > "$1/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
case "$1 $2" in
  "repo view") echo "o/r"; exit 0 ;;
  "pr view") exit 1 ;;
  "api --paginate")
    [ -e "$d/fail_reviews" ] && exit 1
    jq -c '.[]' "$d/reviews.json"; exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
  chmod +x "$1/bin/gh"
}

b64() { printf '%s' "$1" | base64 | tr -d '\n'; }

# body PAYLOAD_JSON — a conductor review body carrying that data payload.
body() {
  printf 'review\n\n<!-- conductor-review -->\n\n<!-- conductor-review-data:v1 %s -->' "$(b64 "$1")"
}

FINDINGS='[{"file":"src/a.rs","line":10,"severity":"warning","reviewer":"Test Coverage","message":"no test for   the  parse-error branch"},
           {"file":"src/b.rs","line":20,"severity":"critical","reviewer":"Security","message":"unquoted path"}]'
payload() { # BLOCKING_JSON [ANSWERED_JSON]
  jq -nc --argjson b "$1" --argjson a "${2:-[]}" \
    '{v: 1, overall_approved: (($b | length) == 0), blocking_findings: $b, answered_findings: $a, summary: "s", scope: "incremental", since: "abc"}'
}

# review ID LOGIN TYPE ASSOC STATE AT BODY
review() {
  jq -nc --argjson id "$1" --arg login "$2" --arg type "$3" --arg assoc "$4" --arg state "$5" --arg at "$6" --arg body "$7" \
    '{id: $id, user: {login: $login, type: $type}, author_association: $assoc, state: $state,
      commit_id: "0123456789abcdef", body: $body, submitted_at: $at}'
}

new_case() { # REVIEW_JSON...
  stub=$(scratch_dir)
  write_stub "$stub"
  if [ "$#" -eq 0 ]; then echo "[]" > "$stub/reviews.json"; else printf '%s\n' "$@" | jq -s '.' > "$stub/reviews.json"; fi
}

BOT='conductor-ai-reviewer[bot]'
run_prev() {
  run_script fetch-previous-round.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" PR_NUMBER=7 TRUSTED_REVIEW_BOTS="$BOT" "$@"
}

case_begin "a trusted bot's review lists its blocking findings"
new_case "$(review 1 "$BOT" Bot NONE CHANGES_REQUESTED 2026-01-01T00:00:00Z "$(body "$(payload "$FINDINGS")")")"
run_prev
assert_exit 0
assert_no_markers
assert_jq '.previous_round | contains("2 blocking finding(s)")'
assert_jq '.previous_round | contains("- [warning] Test Coverage — src/a.rs:10 — no test for the parse-error branch")'
assert_jq '.previous_round | contains("- [critical] Security — src/b.rs:20 — unquoted path")'
assert_jq '.previous_round | contains("on 01234567 (scope incremental)")'

case_begin "answered findings are listed separately"
new_case "$(review 1 "$BOT" Bot NONE COMMENTED 2026-01-01T00:00:00Z "$(body "$(payload '[]' "$FINDINGS")")")"
run_prev
assert_exit 0
assert_jq '.previous_round | contains("All Clear") and contains("Answered by a reply in that round:") and contains("src/a.rs:10")'

case_begin "no conductor review: the first round"
new_case "$(review 1 alice User MEMBER COMMENTED 2026-01-01T00:00:00Z "looks good")"
run_prev
assert_exit 0
assert_jq '.previous_round | startswith("None:")'

case_begin "a dismissed latest review is skipped for the one before it"
new_case \
  "$(review 1 "$BOT" Bot NONE CHANGES_REQUESTED 2026-01-01T00:00:00Z "$(body "$(payload "$FINDINGS")")")" \
  "$(review 2 "$BOT" Bot NONE DISMISSED 2026-01-02T00:00:00Z "$(body "$(payload '[]')")")"
run_prev
assert_exit 0
assert_jq '.previous_round | contains("2 blocking finding(s)")'

case_begin "the latest of several trusted reviews wins"
new_case \
  "$(review 1 "$BOT" Bot NONE CHANGES_REQUESTED 2026-01-01T00:00:00Z "$(body "$(payload "$FINDINGS")")")" \
  "$(review 2 "$BOT" Bot NONE COMMENTED 2026-01-02T00:00:00Z "$(body "$(payload '[]')")")"
run_prev
assert_exit 0
assert_jq '.previous_round | contains("All Clear")'

case_begin "an unlisted bot's or an outsider's review is ignored"
new_case \
  "$(review 1 "$BOT" Bot NONE COMMENTED 2026-01-01T00:00:00Z "$(body "$(payload '[]')")")" \
  "$(review 2 "github-actions[bot]" Bot NONE CHANGES_REQUESTED 2026-01-02T00:00:00Z "$(body "$(payload "$FINDINGS")")")" \
  "$(review 3 mallory User NONE CHANGES_REQUESTED 2026-01-03T00:00:00Z "$(body "$(payload "$FINDINGS")")")"
run_prev
assert_exit 0
assert_jq '.previous_round | contains("All Clear")'

case_begin "the bot is ignored when TRUSTED_REVIEW_BOTS is unset"
new_case "$(review 1 "$BOT" Bot NONE CHANGES_REQUESTED 2026-01-01T00:00:00Z "$(body "$(payload "$FINDINGS")")")"
run_prev TRUSTED_REVIEW_BOTS=
assert_exit 0
assert_jq '.previous_round | startswith("None:")'

case_begin "an unreadable payload: unavailable, the review still runs"
new_case "$(review 1 "$BOT" Bot NONE COMMENTED 2026-01-01T00:00:00Z $'review\n\n<!-- conductor-review -->\n\n<!-- conductor-review-data:v1 !!!notbase64 -->')"
run_prev
assert_exit 0
assert_jq '.previous_round | startswith("Unavailable:")'

case_begin "a non-array answered_findings: unavailable, the review still runs"
new_case "$(review 1 "$BOT" Bot NONE COMMENTED 2026-01-01T00:00:00Z \
  "$(body "$(jq -nc '{v: 1, overall_approved: true, blocking_findings: [], answered_findings: {x: 1}, scope: "full"}')")")"
run_prev
assert_exit 0
assert_jq '.previous_round | startswith("Unavailable:")'

case_begin "odd finding shapes are listed, not fatal"
odd='[{"file":"src/a.rs","line":10,"reviewer":"DRY Abstraction","message":{"text":"not a string"}},
      {"file":null,"line":null,"severity":"warning","message":null},
      "a bare string"]'
new_case "$(review 1 "$BOT" Bot NONE CHANGES_REQUESTED 2026-01-01T00:00:00Z "$(body "$(payload "$odd")")")"
run_prev
assert_exit 0
assert_jq '.previous_round | contains("3 blocking finding(s)")'
assert_jq '.previous_round | contains("- [warning] DRY Abstraction — src/a.rs:10 —")'
assert_jq '.previous_round | contains("- [warning] reviewer — ?:? — ")'
assert_jq '.previous_round | contains("- a bare string")'

case_begin "the reviews can't be fetched: unavailable, the review still runs"
new_case
touch "$stub/fail_reviews"
run_prev
assert_exit 0
assert_jq '.previous_round | startswith("Unavailable:")'

case_begin "no PR for the branch: the first round"
new_case
run_prev PR_NUMBER=
assert_exit 0
assert_jq '.previous_round | startswith("None:")'

finish
