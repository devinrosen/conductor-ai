#!/usr/bin/env bash
# check-pr-ready.sh, wait-for-pr-checks.sh, post-verification.sh and squash-merge.sh end to end against a stub gh (#3407).
set -uo pipefail
source "$(dirname "$0")/lib.sh"

# The stub gh, driven by files in $GH_STUB_DIR:
#   pr.json       what `gh pr view` prints (merged: state/mergeCommit are overridden once `gh pr merge` ran)
#   reviews.json  one JSON array: the PR's reviews
#   checks.json   what `gh pr checks` prints (checks.rc: its exit code, checks.err: its stderr)
#   checks.seq/   instead of checks.json: numbered files (N, N.rc), one consumed per call, the last repeating
#   issues/N      the state of issue N (OPEN unless the file says CLOSED)
#   comments.json existing PR comments (default [])
#   comment       written: the body of `gh pr comment`
#   merge         written: the arguments of `gh pr merge`; merge.body is its body file
#   closed        written: one line per `gh issue close`
#   norepo        written: one line per call (but `repo view`) that came without --repo
#   views         written: the number of each `gh issue view`
#   rm_on_merge   holds a directory that `gh pr merge` removes (a worktree cleaned up under the run)
#   issues/N      FAIL makes `gh issue view N` fail
# Without --repo, a call from a deleted cwd fails like real gh does.
# Any call it doesn't know fails loudly, so a test can't reach the network.
write_stub() { # DIR
  mkdir -p "$1/bin" "$1/issues"
  cat > "$1/bin/gh" <<'STUB'
#!/usr/bin/env bash
d="${GH_STUB_DIR:?}"
has_repo=""
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --repo|-R) has_repo=1; shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
set -- ${args[@]+"${args[@]}"}
if [ -z "$has_repo" ] && [ "$1 $2" != "repo view" ]; then
  echo "$*" >> "$d/norepo"
  if [ ! -d "$PWD" ]; then
    echo "failed to run git: fatal: Unable to read current working directory" >&2
    exit 1
  fi
fi
case "$1 $2" in
  "repo view")
    case "$*" in
      *defaultBranchRef*) echo main ;;
      *) echo "o/r" ;;
    esac
    exit 0 ;;
  "pr list") jq -r '.number' "$d/pr.json"; exit 0 ;;
  "pr view")
    if [ -f "$d/merge" ]; then
      jq -c '.state = "MERGED" | .mergeCommit = {oid: "feedface00000000000000000000000000000000"}' "$d/pr.json"
    else
      cat "$d/pr.json"
    fi
    exit 0 ;;
  "pr checks")
    if [ -d "$d/checks.seq" ]; then
      n=$(cat "$d/checks.n" 2>/dev/null || echo 0)
      last=$(ls "$d/checks.seq" | grep -v '\.rc$' | sort -n | tail -1)
      [ "$n" -gt "$last" ] && n=$last
      cat "$d/checks.seq/$n"
      rc=$(cat "$d/checks.seq/$n.rc" 2>/dev/null || echo 0)
      echo $((n + 1)) > "$d/checks.n"
      exit "$rc"
    fi
    cat "$d/checks.json"
    [ -f "$d/checks.err" ] && cat "$d/checks.err" >&2
    exit "$(cat "$d/checks.rc" 2>/dev/null || echo 0)" ;;
  "api --paginate")
    case "$*" in
      *issues/*/comments*) cat "$d/comments.json" 2>/dev/null || echo '[]' ;;
      *) cat "$d/reviews.json" ;;
    esac
    exit 0 ;;
  "pr comment")
    while [ $# -gt 0 ]; do [ "$1" = "--body-file" ] && cp "$2" "$d/comment"; shift; done
    exit 0 ;;
  "pr merge")
    echo "$*" > "$d/merge"
    [ -f "$d/rm_on_merge" ] && rm -rf "$(cat "$d/rm_on_merge")"
    while [ $# -gt 0 ]; do [ "$1" = "--body-file" ] && cp "$2" "$d/merge.body"; shift; done
    exit 0 ;;
  "issue view")
    echo "$3" >> "$d/views"
    if grep -q FAIL "$d/issues/$3" 2>/dev/null; then echo "gh stub: HTTP 404 not found" >&2; exit 1; fi
    cat "$d/issues/$3" 2>/dev/null || echo OPEN; exit 0 ;;
  "issue close") echo "$3" >> "$d/closed"; echo CLOSED > "$d/issues/$3"; exit 0 ;;
esac
echo "gh stub: unexpected call: $*" >&2
exit 1
STUB
  chmod +x "$1/bin/gh"
}

b64() { printf '%s' "$1" | base64 | tr -d '\n'; }

# review_body APPROVED — a conductor review body with the data payload.
review_body() {
  local payload
  payload=$(jq -nc --argjson a "$1" '{v: 1, overall_approved: $a, blocking_findings: [], summary: "s"}')
  printf 'review\n\n<!-- conductor-review -->\n\n<!-- conductor-review-data:v1 %s -->' "$(b64 "$payload")"
}

# review ID ASSOC STATE COMMIT BODY AT
review() {
  jq -nc --argjson id "$1" --arg assoc "$2" --arg state "$3" --arg commit "$4" --arg body "$5" --arg at "${6:-2026-01-02T00:00:00Z}" \
    '{id: $id, user: {login: "bot"}, author_association: $assoc, state: $state, commit_id: $commit, body: $body, submitted_at: $at}'
}

# new_case — a scratch repo on branch feature (one commit ahead of origin/main) with the
# recipe committed, an approved review on its head, passing checks and an open PR.
new_case() {
  repo=$(scratch_repo)
  origin=$(scratch_dir)
  git init -q --bare "$origin" >/dev/null 2>&1
  cd "$repo" || die "cd failed"
  git remote add origin "$origin"
  commit_file "$repo" README.md base init
  git push -q origin main
  git checkout -q -b feature
  commit_file "$repo" .conductor/verify-recipe.md recipe "recipe"
  head=$(git rev-parse HEAD)
  stub=$(scratch_dir)
  write_stub "$stub"
  jq -nc --arg head "$head" '{number: 7, title: "fix: a thing", body: "## Summary\n\nDoes a thing.\n\nCloses #1\nfixes #2\nCloses other/repo#3\n\n## Test plan\n\nnone",
    isDraft: false, state: "OPEN", baseRefName: "release/0.22.2", headRefName: "fix/a-thing", headRefOid: $head,
    mergeable: "MERGEABLE", reviewDecision: "",
    commits: [{messageBody: "Co-Authored-By: Claude <c@x>\nco-authored-by: Claude <c@x>"}, {messageBody: "Co-Authored-By: Pat <p@x>"}]}' > "$stub/pr.json"
  echo "[$(review 1 MEMBER COMMENTED "$head" "$(review_body true)")]" > "$stub/reviews.json"
  echo '[{"name":"Clippy","bucket":"pass"},{"name":"Test","bucket":"pass"},{"name":"Docs","bucket":"skipping"}]' > "$stub/checks.json"
  # The base branch exists on origin and is contained in feature.
  git push -q origin main:release/0.22.2
}

# set_pr JQ — edit pr.json.
set_pr() { jq -c "$1" "$stub/pr.json" > "$stub/pr.new" && mv "$stub/pr.new" "$stub/pr.json"; }

run_ready() {
  run_script check-pr-ready.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 "$@"
}

case_begin "ready: emits pr_ready and the same-repo linked issues"
new_case
run_ready
assert_exit 0
assert_marker pr_ready
assert_jq ".head_sha == \"$head\" and .pr_number == \"7\" and .base_branch == \"release/0.22.2\""
assert_jq '.linked_issues == "1 2"'
assert_jq '.pr_title == "fix: a thing"'
assert_jq '.required_checks == "Clippy,Test,Docs"'

case_begin "no Closes line: falls back to TICKET_SOURCE_ID"
new_case
set_pr '.body = "no links"'
run_ready TICKET_SOURCE_ID=42
assert_exit 0
assert_jq '.linked_issues == "42"'

case_begin "release/* -> main is refused"
new_case
set_pr '.headRefName = "release/0.22.2" | .baseRefName = "main"'
run_ready
assert_exit 1
assert_stderr_contains "rebase-merged by hand"

case_begin "a draft PR is refused"
new_case
set_pr '.isDraft = true'
run_ready
assert_exit 1
assert_stderr_contains "draft"

case_begin "missing recipe is refused"
new_case
run_ready RECIPE_PATH=.conductor/nope.md
assert_exit 1
assert_stderr_contains "no .conductor/nope.md"

case_begin "local HEAD differs from the PR head: refused"
new_case
set_pr '.headRefOid = "0123456789abcdef0123456789abcdef01234567"'
run_ready
assert_exit 1
assert_stderr_contains "is not the PR head"

case_begin "no conductor review is refused"
new_case
echo '[]' > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "no conductor review"

case_begin "latest review predates the head: refused with the reason"
new_case
echo "[$(review 1 MEMBER COMMENTED "abc1234000000000000000000000000000000000" "$(review_body true)")]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "last conductor review (abc1234) predates head ${head:0:7}"

case_begin "a newer review on an older commit outranks an older review on the head"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" "$(review_body true)" 2026-01-01T00:00:00Z),
      $(review 2 MEMBER COMMENTED "abc1234000000000000000000000000000000000" "$(review_body true)" 2026-01-03T00:00:00Z)]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "predates head"

case_begin "a dismissed review is ignored"
new_case
echo "[$(review 1 MEMBER DISMISSED "$head" "$(review_body true)")]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "no conductor review"

case_begin "a review by an untrusted author is ignored"
new_case
echo "[$(review 1 NONE COMMENTED "$head" "$(review_body true)")]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "no conductor review"

case_begin "a review by a bot listed in TRUSTED_REVIEW_BOTS counts (app reviewers are NONE)"
new_case
jq -nc --arg commit "$head" --arg body "$(review_body true)" \
  '[{id: 1, user: {login: "conductor-ai-reviewer[bot]", type: "Bot"}, author_association: "NONE", state: "APPROVED", commit_id: $commit, body: $body, submitted_at: "2026-01-02T00:00:00Z"}]' > "$stub/reviews.json"
run_ready TRUSTED_REVIEW_BOTS="other[bot], conductor-ai-reviewer[bot]"
assert_exit 0

case_begin "a bot review is ignored when the bot is not listed"
new_case
jq -nc --arg commit "$head" --arg body "$(review_body true)" \
  '[{id: 1, user: {login: "github-actions[bot]", type: "Bot"}, author_association: "NONE", state: "APPROVED", commit_id: $commit, body: $body, submitted_at: "2026-01-02T00:00:00Z"}]' > "$stub/reviews.json"
run_ready TRUSTED_REVIEW_BOTS="conductor-ai-reviewer[bot]"
assert_exit 1
assert_stderr_contains "no conductor review"

case_begin "a listed login that is not a Bot account is not trusted by name"
new_case
jq -nc --arg commit "$head" --arg body "$(review_body true)" \
  '[{id: 1, user: {login: "conductor-ai-reviewer[bot]", type: "User"}, author_association: "NONE", state: "APPROVED", commit_id: $commit, body: $body, submitted_at: "2026-01-02T00:00:00Z"}]' > "$stub/reviews.json"
run_ready TRUSTED_REVIEW_BOTS="conductor-ai-reviewer[bot]"
assert_exit 1
assert_stderr_contains "no conductor review"

case_begin "a review that is not All Clear is refused"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" "$(review_body false)")]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "not All Clear"

case_begin "no payload: the All Clear heading counts"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" $'## Conductor Review Swarm — All Clear\n\n<!-- conductor-review -->')]" > "$stub/reviews.json"
run_ready
assert_exit 0

WITHHELD=$'## Conductor Review Swarm — All Clear\n\n> [!WARNING]\n> **This PR changes the review pipeline that reviewed it.** x\n\n<!-- conductor-review -->'

case_begin "withheld approval: refused without confirm or an approval"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" "$WITHHELD")]" > "$stub/reviews.json"
run_ready
assert_exit 1
assert_stderr_contains "a person must approve"

case_begin "withheld approval: allowed with confirm"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" "$WITHHELD")]" > "$stub/reviews.json"
run_ready CONFIRM=true
assert_exit 0

case_begin "withheld approval: allowed when a person approved"
new_case
echo "[$(review 1 MEMBER COMMENTED "$head" "$WITHHELD")]" > "$stub/reviews.json"
set_pr '.reviewDecision = "APPROVED"'
run_ready
assert_exit 0

case_begin "a failing required check is refused by name"
new_case
echo '[{"name":"Clippy","bucket":"fail"},{"name":"Test","bucket":"pass"}]' > "$stub/checks.json"
run_ready
assert_exit 1
assert_stderr_contains "required check failing: Clippy"

case_begin "a pending required check is refused"
new_case
echo '[{"name":"Clippy","bucket":"pass"},{"name":"Test","bucket":"pending"}]' > "$stub/checks.json"
run_ready
assert_exit 1
assert_stderr_contains "required checks still running: Test"

case_begin "a failing required check with gh exit 1 is still refused by name"
new_case
echo '[{"name":"Clippy","bucket":"fail"}]' > "$stub/checks.json"
echo 1 > "$stub/checks.rc"
run_ready
assert_exit 1
assert_stderr_contains "required check failing: Clippy"

case_begin "a pending required check with gh exit 8 is refused"
new_case
echo '[{"name":"Test","bucket":"pending"}]' > "$stub/checks.json"
echo 8 > "$stub/checks.rc"
run_ready
assert_exit 1
assert_stderr_contains "required checks still running: Test"

case_begin "checks unreadable (no JSON, gh error): refused, not treated as none"
new_case
: > "$stub/checks.json"
echo 1 > "$stub/checks.rc"
echo "HTTP 401: bad credentials" > "$stub/checks.err"
run_ready
assert_exit 1
assert_stderr_contains "could not read required checks"

case_begin "no required checks reported by gh: continues"
new_case
: > "$stub/checks.json"
echo 1 > "$stub/checks.rc"
echo "no required checks reported on the 'x' branch" > "$stub/checks.err"
run_ready
assert_exit 0

case_begin "a conflicting PR is refused"
new_case
set_pr '.mergeable = "CONFLICTING"'
run_ready
assert_exit 1
assert_stderr_contains "merge conflicts"

case_begin "behind the base, dry run: refused and nothing pushed"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
before=$(git -C "$origin" rev-parse feature 2>/dev/null || echo none)
run_ready DRY_RUN=true
assert_exit 1
assert_stderr_contains "does not contain origin/release/0.22.2"
assert_eq "$before" "$(git -C "$origin" rev-parse feature 2>/dev/null || echo none)" "origin feature ref"

case_begin "behind the base: merged in and pushed, then refused for re-review"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
run_ready
assert_exit 1
assert_stderr_contains "merged release/0.22.2 into the branch"
assert_eq "$(git rev-parse HEAD)" "$(git -C "$origin" rev-parse feature)" "pushed head"

case_begin "behind the base, AUTO_REREVIEW=true: merged, pushed, base_merged emitted"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
run_ready AUTO_REREVIEW=true
assert_exit 0
assert_marker base_merged
assert_jq ".head_sha == \"$(git rev-parse HEAD)\" and .head_sha != \"$head\" and .pr_number == \"7\""
assert_jq '.required_checks == "Clippy,Test,Docs" and .linked_issues == "1 2"'
assert_eq "$(git rev-parse HEAD)" "$(git -C "$origin" rev-parse feature)" "pushed head"

case_begin "behind the base, AUTO_REREVIEW=false or an unresolved placeholder: refused as before"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
run_ready AUTO_REREVIEW="{{auto_rereview}}"
assert_exit 1
assert_stderr_contains "merged release/0.22.2 into the branch"

case_begin "AUTO_REREVIEW=true: a draft, a failing check and a stale review still refuse first"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
set_pr '.isDraft = true'
run_ready AUTO_REREVIEW=true
assert_exit 1
assert_stderr_contains "draft"
set_pr '.isDraft = false'
echo '[{"name":"Clippy","bucket":"fail"}]' > "$stub/checks.json"
run_ready AUTO_REREVIEW=true
assert_exit 1
assert_stderr_contains "required check failing: Clippy"
echo '[{"name":"Clippy","bucket":"pass"}]' > "$stub/checks.json"
echo "[$(review 1 MEMBER COMMENTED "abc1234000000000000000000000000000000000" "$(review_body true)")]" > "$stub/reviews.json"
run_ready AUTO_REREVIEW=true
assert_exit 1
assert_stderr_contains "predates head"
assert_eq "$head" "$(git rev-parse HEAD)" "base not merged"

case_begin "behind the base with a conflict: merge aborted, refused"
new_case
git checkout -q main && commit_file "$repo" .conductor/verify-recipe.md other "conflicting base" && git push -q origin main:release/0.22.2 && git checkout -q feature
run_ready
assert_exit 1
assert_stderr_contains "conflicts with release/0.22.2"
assert_eq "" "$(git status --porcelain)" "clean tree after the aborted merge"

case_begin "behind the base, merge fails without a conflict: git's message is kept"
new_case
git checkout -q main && commit_file "$repo" base.txt newer "newer base" && git push -q origin main:release/0.22.2 && git checkout -q feature
mkdir -p "$repo/.git/hooks" && printf '#!/bin/sh\necho "pre-merge hook said no" >&2\nexit 1\n' > "$repo/.git/hooks/pre-merge-commit" && chmod +x "$repo/.git/hooks/pre-merge-commit"
git -C "$repo" config core.hooksPath "$repo/.git/hooks"
run_ready
assert_exit 1
assert_stderr_contains "merging origin/release/0.22.2 failed"
assert_stderr_contains "pre-merge hook said no"
assert_eq "" "$(git status --porcelain)" "clean tree after the aborted merge"

# --- wait-for-pr-checks.sh ---

run_wait() { # [ENV=VAL...]
  run_script wait-for-pr-checks.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 \
    PR_NUMBER=7 POLL_INTERVAL=0 WAIT_TIMEOUT=5 EMPTY_GRACE=1 "$@"
}
# seq N JSON [RC] — checks.seq/N
seq() { mkdir -p "$stub/checks.seq"; echo "$2" > "$stub/checks.seq/$1"; [ -n "${3:-}" ] && echo "$3" > "$stub/checks.seq/$1.rc"; return 0; }
ALL_PASS='[{"name":"Clippy","bucket":"pass"},{"name":"Test","bucket":"pass"}]'

case_begin "wait: pending, then pass: checks_green"
new_case
seq 0 '[{"name":"Clippy","bucket":"pass"},{"name":"Test","bucket":"pending"}]' 8
seq 1 "$ALL_PASS"
run_wait REQUIRED_CHECKS="Clippy,Test"
assert_exit 0
assert_marker checks_green
assert_eq "2" "$(cat "$stub/checks.n")" "polls"

case_begin "wait: pending, then fail: refused naming the check"
new_case
seq 0 '[{"name":"Clippy","bucket":"pending"},{"name":"Test","bucket":"pending"}]' 8
seq 1 '[{"name":"Clippy","bucket":"pass"},{"name":"Test","bucket":"fail"}]' 1
run_wait REQUIRED_CHECKS="Clippy,Test"
assert_exit 1
assert_stderr_contains "required check failing on ${head:0:7}: Test"

case_begin "wait: a required name not yet reported keeps waiting, then passes"
new_case
seq 0 '[{"name":"Clippy","bucket":"pass"}]'
seq 1 "$ALL_PASS"
run_wait REQUIRED_CHECKS="Clippy,Test"
assert_exit 0
assert_marker checks_green
assert_eq "2" "$(cat "$stub/checks.n")" "polls"

case_begin "wait: still pending at the timeout: refused"
new_case
seq 0 '[{"name":"Test","bucket":"pending"}]' 8
run_wait REQUIRED_CHECKS="Test" WAIT_TIMEOUT=0
assert_exit 1
assert_stderr_contains "required checks still running on ${head:0:7} after"
assert_stderr_contains "Test"

case_begin "wait: a required name that never appears times out"
new_case
seq 0 '[{"name":"Clippy","bucket":"pass"}]'
run_wait REQUIRED_CHECKS="Clippy,Test" WAIT_TIMEOUT=0
assert_exit 1
assert_stderr_contains "still running"
assert_stderr_contains "Test"

case_begin "wait: PR head lagging the local head never matches: refused at the timeout"
new_case
set_pr '.headRefOid = "0123456789abcdef0123456789abcdef01234567"'
run_wait WAIT_TIMEOUT=0
assert_exit 1
assert_stderr_contains "is not the local head ${head:0:7}"

case_begin "wait: no checks and none named: green after the grace"
new_case
: > "$stub/checks.json"
echo 1 > "$stub/checks.rc"
echo "no required checks reported on the 'x' branch" > "$stub/checks.err"
run_wait EMPTY_GRACE=0
assert_exit 0
assert_marker checks_green

case_begin "wait: unreadable gh output is refused"
new_case
: > "$stub/checks.json"
echo 1 > "$stub/checks.rc"
echo "HTTP 401: bad credentials" > "$stub/checks.err"
run_wait
assert_exit 1
assert_stderr_contains "could not read required checks"

case_begin "wait: dry run exits 0 without calling gh"
new_case
run_wait DRY_RUN=true
assert_exit 0
assert_marker checks_green

# --- post-verification.sh ---

run_post() { # PRIOR [ENV=VAL...]
  local prior="$1"
  shift
  run_script post-verification.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 \
    PR_NUMBER=7 HEAD_SHA="$head" PRIOR_OUTPUT="$prior" "$@"
}
OK_REPORT='{"verified":true,"issues":[1],"criteria":[{"criterion":"it works","status":"pass","evidence":"ran a|b\nline two"}],"checks":[{"command":"conductor x","result":"pass","excerpt":"ok"}],"run_ids":["01ABC"],"summary":"all good"}'
BAD_REPORT='{"verified":false,"issues":[1],"criteria":[{"criterion":"it works","status":"fail","evidence":"no"}],"summary":"it does not work"}'

case_begin "post-verification: verified posts the report and emits verified"
new_case
run_post "$OK_REPORT"
assert_exit 0
assert_marker verified
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"
assert_eq "yes" "$(grep -q 'conductor-verification' "$stub/comment" && grep -q '01ABC' "$stub/comment" && echo yes || echo no)" "marker and run id in the comment"
assert_eq "yes" "$(grep -q 'ran a\\|b line two' "$stub/comment" && echo yes || echo no)" "evidence on one line, pipe escaped"

case_begin "post-verification: a report already posted for this head is not posted again"
new_case
echo "[{\"author_association\": \"MEMBER\", \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`pass\`.\"}]" > "$stub/comments.json"
run_post "$OK_REPORT"
assert_exit 0
assert_marker verified
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: an earlier failing report at the same head does not suppress a passing one"
new_case
echo "[{\"author_association\": \"MEMBER\", \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`fail\`.\"}]" > "$stub/comments.json"
run_post "$OK_REPORT"
assert_exit 0
assert_marker verified
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a report for an older head does not block the post"
new_case
echo '[{"author_association": "MEMBER", "body": "<!-- conductor-verification -->\nHead: `abc1234`."}]' > "$stub/comments.json"
run_post "$OK_REPORT"
assert_exit 0
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a look-alike report from an untrusted author does not suppress the post"
new_case
echo "[{\"author_association\": \"NONE\", \"user\": {\"login\": \"someone\", \"type\": \"User\"}, \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`pass\`.\"}]" > "$stub/comments.json"
run_post "$OK_REPORT"
assert_exit 0
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a report by a listed bot counts as already posted"
new_case
echo "[{\"author_association\": \"NONE\", \"user\": {\"login\": \"conductor-ai-reviewer[bot]\", \"type\": \"Bot\"}, \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`pass\`.\"}]" > "$stub/comments.json"
run_post "$OK_REPORT" TRUSTED_REVIEW_BOTS="conductor-ai-reviewer[bot]"
assert_exit 0
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: failed verification posts, then exits 1"
new_case
run_post "$BAD_REPORT"
assert_exit 1
assert_stderr_contains "verification failed: it does not work"
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a failing report already posted for this head is not posted again"
new_case
echo "[{\"author_association\": \"MEMBER\", \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`fail\`.\"}]" > "$stub/comments.json"
run_post "$BAD_REPORT"
assert_exit 1
assert_stderr_contains "verification failed: it does not work"
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a failing report posts after an earlier pass at the same head"
new_case
echo "[{\"author_association\": \"MEMBER\", \"body\": \"<!-- conductor-verification -->\\n## x\\n\\nHead: \`${head:0:7}\`. Verdict: \`pass\`.\"}]" > "$stub/comments.json"
run_post "$BAD_REPORT"
assert_exit 1
assert_eq "yes" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"
assert_eq "yes" "$(grep -q 'Verdict: `fail`' "$stub/comment" && echo yes || echo no)" "fail verdict in the comment"

case_begin "post-verification: unreadable report exits 1 without posting"
new_case
run_post "not json"
assert_exit 1
assert_stderr_contains "unreadable"
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a report without criteria is unreadable"
new_case
run_post '{"verified":true}'
assert_exit 1
assert_stderr_contains "unreadable"

case_begin "post-verification: a dirty tree is refused"
new_case
echo x > "$repo/stray.txt"
run_post "$OK_REPORT"
assert_exit 1
assert_stderr_contains "left changes in the worktree"
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

case_begin "post-verification: a moved head is refused"
new_case
commit_file "$repo" more.txt more "more"
run_post "$OK_REPORT"
assert_exit 1
assert_stderr_contains "head moved"

case_begin "post-verification: dry run prints instead of posting"
new_case
run_post "$OK_REPORT" DRY_RUN=true
assert_exit 0
assert_stdout_contains "conductor-verification"
assert_eq "no" "$([ -f "$stub/comment" ] && echo yes || echo no)" "comment posted"

# --- squash-merge.sh ---

run_merge() {
  run_script squash-merge.sh PATH="$stub/bin:$PATH" GH_STUB_DIR="$stub" GH_RETRY_DELAYS=0 \
    PR_NUMBER=7 HEAD_SHA="$head" BASE_BRANCH=release/0.22.2 LINKED_ISSUES="1 2" \
    CLOSE_POLL_INTERVAL=0 CLOSE_POLL_LIMIT=2 "$@"
}

case_begin "squash-merge: subject, body and head pinning"
new_case
echo CLOSED > "$stub/issues/1"; echo CLOSED > "$stub/issues/2"
run_merge
assert_exit 0
assert_marker merged
assert_eq "yes" "$(grep -q -- "--squash --match-head-commit $head --subject fix: a thing (#7)" "$stub/merge" && echo yes || echo no)" "merge arguments"
assert_eq "yes" "$(grep -q '^Does a thing\.$' "$stub/merge.body" && echo yes || echo no)" "summary in the body"
assert_eq "no" "$(grep -q 'Test plan' "$stub/merge.body" && echo yes || echo no)" "only the Summary section"
assert_eq "yes" "$(grep -q '^Closes #1$' "$stub/merge.body" && grep -q '^Closes #2$' "$stub/merge.body" && echo yes || echo no)" "Closes lines"
assert_eq "2" "$(grep -ci '^Co-Authored-By:' "$stub/merge.body")" "deduplicated trailers"
assert_eq "no" "$([ -f "$stub/closed" ] && echo yes || echo no)" "issues the merge already closed are left alone"
assert_eq "no" "$([ -s "$stub/norepo" ] && echo yes || echo no)" "gh calls without --repo"
assert_stdout_contains "still here"

case_begin "squash-merge: an issue still open after polling is closed by hand"
new_case
echo CLOSED > "$stub/issues/1"
run_merge
assert_exit 0
assert_eq "2" "$(cat "$stub/closed")" "hand-closed issue"
assert_stdout_contains "Closed by hand"
assert_eq "no" "$([ -s "$stub/norepo" ] && echo yes || echo no)" "gh calls without --repo"

case_begin "squash-merge: the worktree is removed after the merge and the issues are already closed"
new_case
echo CLOSED > "$stub/issues/1"; echo CLOSED > "$stub/issues/2"
echo "$repo" > "$stub/rm_on_merge"
run_merge CLOSE_POLL_LIMIT=12
scratch_cwd=$(scratch_dir)
cd "$scratch_cwd" || die "cd failed"
assert_exit 0
assert_marker merged
case "$STDERR" in *"could not close"*) fail "unexpected could-not-close warning: $STDERR" ;; esac
assert_stdout_not_contains "still here"
assert_eq "no" "$([ -f "$stub/closed" ] && echo yes || echo no)" "issues closed by hand"
assert_eq "2" "$(wc -l < "$stub/views" | tr -d ' ')" "issue views (one round)"
assert_eq "no" "$([ -s "$stub/norepo" ] && echo yes || echo no)" "gh calls without --repo"
assert_stdout_contains "already removed"

case_begin "squash-merge: issue view failures give up early"
new_case
echo CLOSED > "$stub/issues/1"; echo FAIL > "$stub/issues/2"
run_merge CLOSE_POLL_LIMIT=10 CLOSE_VIEW_FAIL_LIMIT=3
assert_exit 0
assert_stderr_contains "stopped waiting"
assert_eq "3" "$(grep -c '^2$' "$stub/views")" "views of the failing issue"
assert_eq "2" "$(cat "$stub/closed")" "close attempted for the unreadable issue"

case_begin "squash-merge: a head that changed is refused"
new_case
set_pr '.headRefOid = "0123456789abcdef0123456789abcdef01234567"'
run_merge
assert_exit 1
assert_stderr_contains "head changed"
assert_eq "no" "$([ -f "$stub/merge" ] && echo yes || echo no)" "merge called"

case_begin "squash-merge: a closed PR is refused"
new_case
set_pr '.state = "CLOSED"'
run_merge
assert_exit 1
assert_stderr_contains "no longer open"

case_begin "squash-merge: a resume after the merge skips it and still closes issues"
new_case
set_pr '.state = "MERGED" | .mergeCommit = {oid: "feedface00000000000000000000000000000000"}'
echo CLOSED > "$stub/issues/1"
run_merge
assert_exit 0
assert_marker merged
assert_eq "no" "$([ -f "$stub/merge" ] && echo yes || echo no)" "merge called"
assert_eq "2" "$(cat "$stub/closed")" "hand-closed issue"
assert_eq "no" "$([ -s "$stub/norepo" ] && echo yes || echo no)" "gh calls without --repo"

case_begin "squash-merge: a PR merged at another head is refused"
new_case
set_pr '.state = "MERGED" | .headRefOid = "0123456789abcdef0123456789abcdef01234567"'
run_merge
assert_exit 1
assert_stderr_contains "head changed"

case_begin "squash-merge: dry run prints the message and merges nothing"
new_case
run_merge DRY_RUN=true
assert_exit 0
assert_stdout_contains "Subject: fix: a thing (#7)"
assert_stdout_contains "Closes #1"
assert_eq "no" "$([ -f "$stub/merge" ] && echo yes || echo no)" "merge called"

finish
