#!/usr/bin/env bash
# reuse-review.sh and review-scope.sh against a stub gh: who counts as a trusted reviewer (#3421).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# The stub gh, driven by files in $GH_STUB_DIR:
#   review.json   what `gh api repos/o/r/pulls/7/reviews/ID` prints
#   reviews.json  one JSON array: the PR's reviews (`gh api --paginate .../reviews`, which the
#                 script reads with `--jq '.[]'`, so the stub prints one review per line)
# Any call it doesn't know fails loudly, so a test can't reach the network.
write_stub() { # DIR
  mkdir -p "$1/bin"
  cat > "$1/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
case "$1 $2" in
  "repo view") echo "o/r"; exit 0 ;;
  "api --paginate") jq -c '.[]' "$d/reviews.json"; exit 0 ;;
esac
case "$1 $2" in
  "api repos/o/r/pulls/7/reviews/"*) cat "$d/review.json"; exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
  chmod +x "$1/bin/gh"
}

b64() { printf '%s' "$1" | base64 | tr -d '\n'; }

# review_body — a conductor review body with the data payload submit-review writes.
review_body() {
  local payload
  payload=$(jq -nc '{v: 1, overall_approved: true, blocking_findings: [], summary: "s", scope: "full", since: ""}')
  printf 'review\n\n<!-- conductor-review -->\n\n<!-- conductor-review-data:v1 %s -->' "$(b64 "$payload")"
}

# review LOGIN TYPE ASSOC — an approved conductor review on HEAD.
review() {
  jq -nc --arg login "$1" --arg type "$2" --arg assoc "$3" --arg commit "$head" --arg body "$(review_body)" \
    '{id: 55, user: {login: $login, type: $type}, author_association: $assoc, state: "COMMENTED",
      commit_id: $commit, body: $body, submitted_at: "2026-01-02T00:00:00Z"}'
}

# new_case LOGIN TYPE ASSOC — a scratch repo whose HEAD the stubbed review covers.
new_case() {
  repo=$(scratch_dir)
  git -C "$repo" init -q >/dev/null 2>&1 || die "git init failed"
  git -C "$repo" symbolic-ref HEAD refs/heads/main
  git -C "$repo" config commit.gpgsign false
  git -C "$repo" config core.hooksPath /dev/null
  commit_file "$repo" README.md base init
  cd "$repo" || die "cd failed"
  head=$(git rev-parse HEAD)
  stub=$(scratch_dir)
  write_stub "$stub"
  review "$1" "$2" "$3" > "$stub/review.json"
  echo "[$(review "$1" "$2" "$3")]" > "$stub/reviews.json"
}

run_reuse() {
  run_script reuse-review.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 PR_NUMBER=7 REVIEW_ID=55 "$@"
}
run_scope() {
  run_script review-scope.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" PR_NUMBER=7 "$@"
}

BOT='conductor-ai-reviewer[bot]'

case_begin "reuse-review: a listed bot's review on HEAD is reused"
new_case "$BOT" Bot NONE
run_reuse TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 0
assert_marker approved

case_begin "reuse-review: a list with spaces and other bots still matches"
new_case "$BOT" Bot NONE
run_reuse TRUSTED_REVIEW_BOTS="other[bot],  ${BOT} "
assert_exit 0
assert_marker approved

case_begin "reuse-review: the bot is refused when TRUSTED_REVIEW_BOTS is unset"
new_case "$BOT" Bot NONE
run_reuse
assert_exit 1
assert_stderr_contains "not from a trusted author"

case_begin "reuse-review: an unlisted bot is refused"
new_case "github-actions[bot]" Bot NONE
run_reuse TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 1
assert_stderr_contains "not from a trusted author"

case_begin "reuse-review: a User with the bot's login is refused"
new_case "$BOT" User NONE
run_reuse TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 1
assert_stderr_contains "not from a trusted author"

case_begin "reuse-review: a MEMBER's review is still reused"
new_case alice User MEMBER
run_reuse
assert_exit 0
assert_marker approved

case_begin "review-scope: a listed bot's review on HEAD is reused"
new_case "$BOT" Bot NONE
run_scope TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 0
assert_marker reuse_review
assert_jq '.reuse_review_id == "55" and .review_scope == "full"'

case_begin "review-scope: the bot with TRUSTED_REVIEW_BOTS unset gets a full review"
new_case "$BOT" Bot NONE
run_scope
assert_exit 0
assert_no_markers
assert_jq '.review_scope == "full" and .reuse_review_id == ""'

case_begin "review-scope: an unlisted bot gets a full review"
new_case "github-actions[bot]" Bot NONE
run_scope TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 0
assert_no_markers

case_begin "review-scope: a User with the bot's login gets a full review"
new_case "$BOT" User NONE
run_scope TRUSTED_REVIEW_BOTS="$BOT"
assert_exit 0
assert_no_markers

case_begin "review-scope: a COLLABORATOR's review is still reused"
new_case alice User COLLABORATOR
run_scope
assert_exit 0
assert_marker reuse_review

finish
