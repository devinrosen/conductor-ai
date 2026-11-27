#!/usr/bin/env bash
# check-pr-ready.sh — first step of verify-and-merge (#3407). Refuses, with a one-line
# reason on stderr and exit 1, unless the current branch's PR is ready to be verified
# and merged. The reason becomes the step error.
# Env vars: CONFIRM (the workflow's confirm input; must be true or false), DRY_RUN, TICKET_SOURCE_ID (fallback
# for the linked issue), RECIPE_PATH (default .conductor/verify-recipe.md),
# AUTO_REREVIEW (must be true or false; "true" turns on the base-merge path below),
# WAIT_FOR_CHECKS (must be true or false; "true" emits checks_pending instead of refusing
# when required checks are still running, #3480),
# TRUSTED_REVIEW_BOTS (comma-separated bot logins whose reviews are trusted, e.g. the
# repo's conductor reviewer GitHub App; GitHub reports such a bot as author_association
# NONE, so without this its reviews never count).
#
# Checks, in order: an open non-draft PR; not a release/* -> default branch PR; the
# repo's verify recipe exists; the local HEAD is the PR head; the latest trusted,
# non-dismissed conductor review is on the head and All Clear (a withheld approval
# needs a person's APPROVE, or confirm); required checks pass; no conflicts; the
# branch contains its base (otherwise the base is merged in and pushed; the new head is
# unreviewed, so with AUTO_REREVIEW=true it emits base_merged and the workflow re-reviews
# and waits for CI, and otherwise the run stops).
#
# With AUTO_REREVIEW=true a head that an earlier run (or a person) already moved is
# treated the same way when every commit since the last review is a merge of the base
# branch: the stale review and still-running required checks don't refuse, and the script
# emits base_merged for the current head (#3452). Any other commit since the review, a
# failing check or a review that isn't All Clear still refuses. recheck-pr-ready runs with
# AUTO_REREVIEW=false and refuses both, so there is only one automatic round.
#
# With WAIT_FOR_CHECKS=true an approved head whose only blocker is still-running required
# checks emits checks_pending: the workflow waits with wait-for-pr-checks.sh and runs this
# script again with WAIT_FOR_CHECKS=false (and AUTO_REREVIEW=false), so a check that is still
# pending then refuses. A base merge takes priority over checks_pending: it replaces the head.
#
# On success emits pr_ready (or base_merged or checks_pending) plus pr_number, pr_title, base_branch,
# head_sha, linked_issues (space-separated same-repo issue numbers from
# Closes/Fixes/Resolves) and required_checks (comma-separated names seen before a merge).
set -euo pipefail

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"
# shellcheck source=lib-trusted-review.sh
source "$(dirname "$0")/lib-trusted-review.sh"
# shellcheck source=lib-git-retry.sh
source "$(dirname "$0")/lib-git-retry.sh"
# shellcheck source=lib-pr-checks.sh
source "$(dirname "$0")/lib-pr-checks.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

is_true() { [ "${1:-}" = "true" ]; }

# base_merge_only REVIEW_COMMIT HEAD BASE: true when REVIEW_COMMIT is an ancestor of HEAD
# and every first-parent commit after it is a merge whose other parents are all on
# origin/BASE, i.e. the head differs from the review only by merges of the base.
base_merge_only() {
  local r="$1" head="$2" base="$3" line c first p oldest="" oldest_first="" out
  [ -n "${r}" ] || refuse "the last conductor review has no commit id, so the head ${head:0:7} can't be checked against it"
  git cat-file -e "${r}^{commit}" 2>/dev/null \
    || refuse "the last conductor review's commit ${r:0:7} is not in this checkout, so the head ${head:0:7} can't be checked against it"
  # Git errors refuse with their own message; only a genuine non-base-merge head returns 1.
  git merge-base --is-ancestor "${r}" "${head}" \
    || refuse "last conductor review (${r:0:7}) predates head ${head:0:7} and is not an ancestor of it (branch rebased or force-pushed?)"
  out=$(git rev-list --first-parent --parents "${r}..${head}" 2>&1) \
    || refuse "could not list commits ${r:0:7}..${head:0:7}: ${out}"
  while read -r -a line; do
    c="${line[0]}"
    [ "${#line[@]}" -ge 3 ] || return 1
    first="${line[1]}"
    for p in "${line[@]:2}"; do
      git merge-base --is-ancestor "${p}" "origin/${base}" || return 1
    done
    oldest="${c}"
    oldest_first="${first}"
  done <<<"${out}"
  [ -n "${oldest}" ] && [ "${oldest_first}" = "${r}" ]
}

# emit MARKER SHA CONTEXT: the base_merged / checks_pending result (see the header).
emit() {
  jq -n --arg n "${PR_NUMBER}" --arg title "${PR_TITLE}" --arg base "${BASE_BRANCH}" \
    --arg marker "$1" --arg sha "$2" --arg ctx "$3" --arg issues "${LINKED}" --arg req "${REQUIRED_NAMES}" '
    {markers: [$marker], context: $ctx,
     pr_number: $n, pr_title: $title, base_branch: $base, head_sha: $sha, linked_issues: $issues, required_checks: $req}' \
    | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
}
STALE_BASE_MERGE=""
CHECKS_PENDING=""
PENDING=""
# CONFIRM (a boolean input) and AUTO_REREVIEW (an input with a default) always have a
# value when the workflow's inputs resolve. The engine turns an unresolved placeholder
# into an empty string, so an empty value means the input wasn't passed: refuse rather
# than read it as "off" (#3449, #3454).
CONFIRM="${CONFIRM:-}"
AUTO_REREVIEW="${AUTO_REREVIEW:-}"
WAIT_FOR_CHECKS="${WAIT_FOR_CHECKS:-}"
case "${CONFIRM}" in true | false) ;; *) refuse "CONFIRM must be true or false, got '${CONFIRM}' (the workflow's confirm input has no value)" ;; esac
case "${AUTO_REREVIEW}" in true | false) ;; *) refuse "AUTO_REREVIEW must be true or false, got '${AUTO_REREVIEW}' (the workflow's auto_rereview input has no value)" ;; esac
case "${WAIT_FOR_CHECKS}" in true | false) ;; *) refuse "WAIT_FOR_CHECKS must be true or false, got '${WAIT_FOR_CHECKS}' (the workflow's wait_for_checks input has no value)" ;; esac
# dry_run is an engine built-in, always set when run by the workflow engine.
DRY_RUN="${DRY_RUN:-}"
# ticket_source_id is injected only when the run has a ticket; it isn't a declared input.
[[ "${TICKET_SOURCE_ID:-}" == *"{{"* ]] && TICKET_SOURCE_ID=""

# 1. The PR for this branch.
BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
{ [ -n "${BRANCH}" ] && [ "${BRANCH}" != "HEAD" ]; } || refuse "not on a named branch"
LIST=$(gh_retry gh pr list --head "${BRANCH}" --state open --json number -q '.[0].number // empty') \
  || refuse "could not list PRs for ${BRANCH}"
[[ "${LIST}" =~ ^[0-9]+$ ]] || refuse "no open PR for branch ${BRANCH}"
PR_NUMBER="${LIST}"

PR=$(gh_retry gh pr view "${PR_NUMBER}" \
  --json number,title,body,isDraft,state,baseRefName,headRefName,headRefOid,mergeable,reviewDecision) \
  || refuse "could not read PR #${PR_NUMBER}"
pr() { printf '%s' "${PR}" | jq -r "$1"; }

[ "$(pr '.state')" = "OPEN" ] || refuse "PR #${PR_NUMBER} is not open"
[ "$(pr '.isDraft')" = "false" ] || refuse "PR #${PR_NUMBER} is a draft"
BASE_BRANCH=$(pr '.baseRefName')
HEAD_BRANCH=$(pr '.headRefName')
PR_HEAD=$(pr '.headRefOid')
PR_TITLE=$(pr '.title')

# 2. release/* -> default branch is a manual rebase-merge.
DEFAULT_BRANCH=$(gh_retry gh repo view --json defaultBranchRef -q .defaultBranchRef.name) \
  || refuse "could not resolve the default branch"
case "${HEAD_BRANCH}" in
  release/*)
    if [ "${BASE_BRANCH}" = "${DEFAULT_BRANCH}" ]; then
      refuse "${HEAD_BRANCH} -> ${DEFAULT_BRANCH} PRs are rebase-merged by hand, then finish.sh"
    fi ;;
esac

# 3. The repo's recipe.
RECIPE_PATH="${RECIPE_PATH:-.conductor/verify-recipe.md}"
[ -f "${RECIPE_PATH}" ] || refuse "no ${RECIPE_PATH}; add one (see docs/workflow/verify-and-merge.md)"

# 4. The conductor review on the head.
LOCAL_HEAD=$(git rev-parse HEAD)
[ "${LOCAL_HEAD}" = "${PR_HEAD}" ] || refuse "local branch (${LOCAL_HEAD:0:7}) is not the PR head (${PR_HEAD:0:7}); pull or push first"

OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) || refuse "could not resolve the repository"
REVIEWS=$(gh_retry gh api --paginate "repos/${OWNER_REPO}/pulls/${PR_NUMBER}/reviews" | jq -s 'add // []') \
  || refuse "could not list the reviews of PR #${PR_NUMBER}"
# Trusted: an OWNER/MEMBER/COLLABORATOR author, or a Bot listed in TRUSTED_REVIEW_BOTS
# (the rule lives in lib-trusted-review.sh).
REVIEW=$(printf '%s' "${REVIEWS}" | jq -c --arg bots "${TRUSTED_REVIEW_BOTS:-}" "$(trusted_author_jq)"'
  [ .[] | select((.body // "") | contains("<!-- conductor-review -->"))
        | select(.state != "DISMISSED")
        | select(trusted_author($bots)) ]
  | sort_by(.submitted_at // "") | last // empty')
[ -n "${REVIEW}" ] || refuse "no conductor review by a trusted author on PR #${PR_NUMBER}"
REVIEW_COMMIT=$(printf '%s' "${REVIEW}" | jq -r '.commit_id // ""')
if [ "${REVIEW_COMMIT}" != "${PR_HEAD}" ]; then
  # An earlier run that merged the base leaves a head nothing re-reviewed (#3452).
  if is_true "${AUTO_REREVIEW}"; then
    FETCH_ERR=$(git fetch --quiet origin "${BASE_BRANCH}" 2>&1) \
      || refuse "could not fetch origin/${BASE_BRANCH} to check the head against the review: ${FETCH_ERR}"
    if base_merge_only "${REVIEW_COMMIT}" "${PR_HEAD}" "${BASE_BRANCH}"; then
      STALE_BASE_MERGE=1
    fi
  fi
  if [ -z "${STALE_BASE_MERGE}" ]; then
    refuse "last conductor review (${REVIEW_COMMIT:0:7}) predates head ${PR_HEAD:0:7}"
  fi
fi

APPROVED=$(printf '%s' "${REVIEW}" | jq -r '
  (.body // "") as $b
  | ( [ try ($b | capture("<!-- conductor-review-data:v1 (?<d>[A-Za-z0-9+/=]+) -->").d | @base64d | fromjson | .overall_approved) catch empty ]
      | .[0] ) as $payload
  | if ($payload | type) == "boolean" then $payload
    else ($b | contains("Conductor Review Swarm — All Clear")) end')
[ "${APPROVED}" = "true" ] || refuse "last conductor review (${REVIEW_COMMIT:0:7}) is not All Clear"

# submit-review posts an All Clear as a COMMENT, not an APPROVE, when the PR changes
# the review pipeline: a person must approve it.
if printf '%s' "${REVIEW}" | jq -e '(.body // "") | contains("changes the review pipeline") or contains("Couldn'"'"'t check whether this PR changes the review pipeline")' >/dev/null; then
  if [ "$(pr '.reviewDecision // ""')" != "APPROVED" ] && ! is_true "${CONFIRM}"; then
    refuse "review pipeline changed; a person must approve, or re-run with confirm=true"
  fi
fi

# 5. Required checks (read by lib-pr-checks.sh: gh's JSON counts whatever its exit code).
read_required_checks "${PR_NUMBER}" || refuse "${CHECKS_ERROR}"
REQUIRED_NAMES=$(printf '%s' "${CHECKS}" | jq -r '[.[].name] | join(",")')
if [ "$(printf '%s' "${CHECKS}" | jq 'length')" -eq 0 ]; then
  echo "Warning: no required checks reported for PR #${PR_NUMBER}; continuing." >&2
else
  FAILING=$(printf '%s' "${CHECKS}" | jq -r '[.[] | select(.bucket == "fail" or .bucket == "cancel") | .name] | join(", ")')
  [ -z "${FAILING}" ] || refuse "required check failing: ${FAILING}"
  PENDING=$(printf '%s' "${CHECKS}" | jq -r '[.[] | select(.bucket == "pending") | .name] | join(", ")')
  if [ -n "${PENDING}" ]; then
    if [ -n "${STALE_BASE_MERGE}" ]; then
      echo "Note: required checks still running (${PENDING}); wait-for-pr-checks waits for them." >&2
    elif is_true "${WAIT_FOR_CHECKS}"; then
      CHECKS_PENDING=1
      echo "Note: required checks still running (${PENDING}); wait-for-pending-checks waits for them." >&2
    else
      refuse "required checks still running: ${PENDING}"
    fi
  fi
fi

# 6. Conflicts.
[ "$(pr '.mergeable')" != "CONFLICTING" ] || refuse "PR #${PR_NUMBER} has merge conflicts with ${BASE_BRANCH}; run rebase-and-fix"

# Linked issues: the rule close-linked-issues.yml uses, same-repo only.
LINKED=$(pr '.body // ""' | tr -d '\r' | grep -oiE '(closes|fixes|resolves)[[:space:]]+#[0-9]+' | grep -oE '[0-9]+' | sort -un | tr '\n' ' ' | sed 's/ $//' || true)
if [ -z "${LINKED}" ] && [[ "${TICKET_SOURCE_ID:-}" =~ ^[0-9]+$ ]]; then
  LINKED="${TICKET_SOURCE_ID}"
fi

# 7. The branch must contain its base.
git fetch --quiet origin "${BASE_BRANCH}" || refuse "could not fetch origin/${BASE_BRANCH}"
if ! git merge-base --is-ancestor "origin/${BASE_BRANCH}" HEAD; then
  if is_true "${DRY_RUN}"; then
    refuse "branch does not contain origin/${BASE_BRANCH} (dry run: not merging it in)"
  fi
  if ! MERGE_OUT=$(git merge --no-edit "origin/${BASE_BRANCH}" 2>&1); then
    # Unmerged paths mean a real conflict; anything else (no git identity, a hook,
    # unrelated histories, a dirty index) keeps git's own message.
    CONFLICTED=$(git ls-files -u | head -1)
    git merge --abort >/dev/null 2>&1 || true
    if [ -n "${CONFLICTED}" ]; then
      refuse "branch conflicts with ${BASE_BRANCH}; run rebase-and-fix"
    fi
    refuse "merging origin/${BASE_BRANCH} failed: $(printf '%s' "${MERGE_OUT}" | tr '\n' ' ' | head -c 300)"
  fi
  git_push_retry origin HEAD >/dev/null || refuse "merged ${BASE_BRANCH} into the branch but the push failed"
  NEW_HEAD=$(git rev-parse HEAD)
  if is_true "${AUTO_REREVIEW}"; then
    # Every other check passed: the merge is the only thing wrong. The workflow re-reviews
    # the new head, waits for its checks and checks again (#3436).
    emit base_merged "${NEW_HEAD}" "merged ${BASE_BRANCH} into the branch (new head ${NEW_HEAD:0:7}); re-reviewing with iterate-pr"
    exit 0
  fi
  refuse "merged ${BASE_BRANCH} into the branch (new head ${NEW_HEAD:0:7}); re-review with iterate-pr, then re-run verify-and-merge (or leave auto_rereview on)"
fi

if [ -n "${STALE_BASE_MERGE}" ]; then
  emit base_merged "${PR_HEAD}" "head ${PR_HEAD:0:7} differs from the last review (${REVIEW_COMMIT:0:7}) only by merges of ${BASE_BRANCH}; re-reviewing with iterate-pr"
  exit 0
fi

if [ -n "${CHECKS_PENDING}" ]; then
  emit checks_pending "${PR_HEAD}" "PR #${PR_NUMBER} is reviewed on ${PR_HEAD:0:7}; waiting for required checks: ${PENDING}"
  exit 0
fi

jq -n --arg n "${PR_NUMBER}" --arg title "${PR_TITLE}" --arg base "${BASE_BRANCH}" \
  --arg sha "${PR_HEAD}" --arg issues "${LINKED}" --arg req "${REQUIRED_NAMES}" '
  {markers: ["pr_ready"],
   context: "PR #\($n) is ready to verify: reviewed on \($sha[0:7]), required checks green, base \($base).",
   pr_number: $n, pr_title: $title, base_branch: $base, head_sha: $sha, linked_issues: $issues, required_checks: $req}' \
  | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
