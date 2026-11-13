#!/usr/bin/env bash
# submit-review.sh end to end against a stub gh: the posted-review lookup (#3331).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# The stub gh, driven by files in $GH_STUB_DIR:
#   user          login `gh api user` prints; without the file it fails like an App token (403)
#   author        the PR author's login
#   poster        JSON user of reviews posted by `gh pr review` (default: the conductor bot)
#   reviews.json  the PR's reviews, one JSON array per page; `gh pr review` appends to the last page
#   drop_post     when present, `gh pr review` posts nothing
#   posted        written: the review flag of each `gh pr review`
# Any call it doesn't know fails loudly, so a test can't reach the network.
write_stub() { # DIR
  mkdir -p "$1/bin"
  cat > "$1/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
case "$1 $2" in
  "repo view") echo "o/r"; exit 0 ;;
  "api user")
    if [ -f "$d/user" ]; then cat "$d/user"; exit 0; fi
    echo "gh: Resource not accessible by integration (HTTP 403)" >&2; exit 1 ;;
  "pr view") cat "$d/author"; exit 0 ;;
  "api --paginate")
    [ -f "$d/reviews.json" ] || echo '[]' > "$d/reviews.json"
    if [ "${4:-}" = "--jq" ]; then jq -r "$5" "$d/reviews.json"; else cat "$d/reviews.json"; fi
    exit 0 ;;
  "api --method") exit 0 ;;
  "pr review")
    flag="$4"
    while [ $# -gt 0 ]; do [ "$1" = "--body-file" ] && body_file="$2"; shift; done
    echo "$flag" >> "$d/posted"
    [ -f "$d/drop_post" ] && exit 0
    n=$(cat "$d/counter" 2>/dev/null || echo 5000)
    echo $((n + 1)) > "$d/counter"
    poster='{"login":"conductor-ai-reviewer[bot]","type":"Bot"}'
    [ -f "$d/poster" ] && poster=$(cat "$d/poster")
    [ -f "$d/reviews.json" ] || echo '[]' > "$d/reviews.json"
    jq -c -s --argjson id "$n" --argjson user "$poster" --rawfile body "$body_file" \
      '{id: $id, user: $user, body: $body, submitted_at: (now | floor | todate),
        html_url: "https://github.com/o/r/pull/7#pullrequestreview-\($id)"} as $r
       | (if length == 0 then [[$r]] else (.[-1] += [$r]) end) | .[]' \
      "$d/reviews.json" > "$d/reviews.new" && mv "$d/reviews.new" "$d/reviews.json"
    exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
  chmod +x "$1/bin/gh"
}

PRIOR='{"overall_approved":false,"reviewed_by":"Correctness","summary":"s",
  "blocking_findings":[{"file":"src/a.txt","line":1,"severity":"critical","reviewer":"Correctness","message":"m"}],
  "off_diff_findings":[]}'

# new_case — scratch repo with a non-pipeline change, a stub dir in $stub, and cd into the repo.
new_case() {
  repo=$(scratch_repo)
  cd "$repo" || die "cd failed"
  commit_file "$repo" README.md base init
  git update-ref refs/remotes/origin/main HEAD
  git checkout -q -b feature
  commit_file "$repo" src/a.txt change "change"
  stub=$(scratch_dir)
  write_stub "$stub"
  echo bob > "$stub/author"
}

run_submit() {
  run_script submit-review.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 \
    PR_NUMBER=7 BASE_BRANCH=main FILE_OFF_DIFF=false PRIOR_OUTPUT="$PRIOR"
}

# review ID LOGIN TYPE SUBMITTED_AT [BODY] — a review object.
review() {
  jq -nc --argjson id "$1" --arg login "$2" --arg type "$3" --arg at "$4" \
    --arg body "${5:-<!-- conductor-review -->}" \
    '{id: $id, user: {login: $login, type: $type}, body: $body, submitted_at: $at,
      html_url: "https://github.com/o/r/pull/7#pullrequestreview-\($id)"}'
}
OLD="2020-01-01T00:00:00Z"

case_begin "bot poster, gh api user fails: the posted review is recorded"
new_case
echo "[$(review 100 'conductor-ai-reviewer[bot]' Bot "$OLD")]" > "$stub/reviews.json"
run_submit
assert_exit 0
assert_jq '.review_id == 5000'
assert_jq '.review_url | endswith("pullrequestreview-5000")'
assert_jq '.event == "REQUEST_CHANGES"'

case_begin "plain-user poster: the posted review is recorded"
new_case
echo alice > "$stub/user"
echo '{"login":"alice","type":"User"}' > "$stub/poster"
run_submit
assert_exit 0
assert_jq '.review_id == 5000'
assert_jq '.event == "REQUEST_CHANGES"'

case_begin "own PR: COMMENT event, review still recorded"
new_case
echo alice > "$stub/user"
echo alice > "$stub/author"
echo '{"login":"alice","type":"User"}' > "$stub/poster"
run_submit
assert_exit 0
assert_jq '.review_id == 5000'
assert_jq '.event == "COMMENT"'

case_begin "only an older conductor review: nothing recorded, the context says so"
new_case
touch "$stub/drop_post"
echo "[$(review 100 'conductor-ai-reviewer[bot]' Bot "$OLD")]" > "$stub/reviews.json"
run_submit
assert_exit 0
assert_jq '.review_id == null'
assert_jq '.review_url == null'
assert_context_contains "could not be looked up"
assert_stderr_contains "could not look up the URL"

case_begin "a bot review outranks a newer human marker review in the window"
new_case
future=$(jq -nr '(now + 60) | floor | todate')
echo "[$(review 999999 carol User "$future")]" > "$stub/reviews.json"
run_submit
assert_exit 0
assert_jq '.review_id == 5000'

case_begin "paginated reviews: the review on the last page is found"
new_case
{
  echo "[$(review 100 'conductor-ai-reviewer[bot]' Bot "$OLD")]"
  echo "[$(review 101 carol User "$OLD" 'no marker')]"
} > "$stub/reviews.json"
run_submit
assert_exit 0
assert_jq '.review_id == 5000'
assert_jq '.review_url | endswith("pullrequestreview-5000")'

finish
