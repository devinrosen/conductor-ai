#!/usr/bin/env bash
# squash-merge.sh — last step of verify-and-merge (#3407): squash-merges the verified PR.
# Env vars: PR_NUMBER, HEAD_SHA, BASE_BRANCH, LINKED_ISSUES (space-separated numbers),
# DRY_RUN (print the planned commit message only), CLOSE_POLL_INTERVAL (default 15 s)
# and CLOSE_POLL_LIMIT (default 12 polls) for the issue-closing wait; CLOSE_VIEW_FAIL_LIMIT
# (default 3) is how many polling rounds in a row may fail to read an issue before it gives up.
# Every gh call passes --repo, resolved once up front: the worktree can be removed under
# the run once the PR merges, and gh would then fail to find the repo from the cwd (#3414).
#
# The commit subject is "<title> (#PR)"; the body is the PR's Summary section, a Closes
# line per linked issue and the commits' Co-Authored-By trailers, deduplicated.
# --match-head-commit means a push after verification is never merged. The branch is
# not deleted (it is checked out by this worktree), and neither is the worktree: the
# run is executing in it, so the cleanup command is printed instead (only while the
# worktree is still there).
set -euo pipefail

START_DIR="${PWD}"

# shellcheck source=lib-gh-retry.sh
source "$(dirname "$0")/lib-gh-retry.sh"

refuse() { echo "verify-and-merge: $*" >&2; exit 1; }

[[ "${PR_NUMBER:-}" =~ ^[0-9]+$ ]] || refuse "no valid PR number"
[[ "${HEAD_SHA:-}" =~ ^[0-9a-f]{7,40}$ ]] || refuse "no valid head SHA"
for v in BASE_BRANCH LINKED_ISSUES DRY_RUN; do
  case "${!v:-}" in *"{{"*) eval "${v}=''" ;; esac
done
ISSUES="${LINKED_ISSUES:-}"

OWNER_REPO=$(gh_retry gh repo view --json nameWithOwner -q .nameWithOwner) \
  || refuse "could not resolve the repository"

PR=$(gh_retry gh pr view "${PR_NUMBER}" --repo "${OWNER_REPO}" --json title,body,state,headRefOid,baseRefName,commits) \
  || refuse "could not read PR #${PR_NUMBER}"

# A resume after gh pr merge succeeded finds the PR MERGED at the verified head: skip
# the merge and go on to the issue closing.
ALREADY_MERGED=""
if [ "${DRY_RUN:-}" != "true" ]; then
  STATE=$(echo "${PR}" | jq -r '.state')
  [ "$(echo "${PR}" | jq -r '.headRefOid')" = "${HEAD_SHA}" ] \
    || refuse "PR #${PR_NUMBER} head changed since verification; re-run verify-and-merge"
  case "${STATE}" in
    OPEN) ;;
    MERGED) ALREADY_MERGED=1; echo "PR #${PR_NUMBER} is already merged at ${HEAD_SHA:0:7}; resuming after the merge." >&2 ;;
    *) refuse "PR #${PR_NUMBER} is no longer open" ;;
  esac
fi

SUBJECT="$(echo "${PR}" | jq -r '.title') (#${PR_NUMBER})"

# The "## Summary" section pr-body.sh writes, else the first paragraph.
SUMMARY=$(echo "${PR}" | jq -r '.body // ""' | tr -d '\r' | awk '
  /^## Summary[[:space:]]*$/ { s = 1; next }
  s && /^## / { exit }
  s { print }' | sed -e '/./,$!d')
if [ -z "${SUMMARY}" ]; then
  SUMMARY=$(echo "${PR}" | jq -r '.body // ""' | tr -d '\r' | awk 'NF { p = 1 } p && !NF { exit } p { print }')
fi
SUMMARY=$(printf '%s' "${SUMMARY}" | awk 'BEGIN { RS = "" } { sub(/[[:space:]]+$/, ""); print }')

BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/squash_body.XXXXXX")
trap 'rm -f "${BODY_FILE}"' EXIT
{
  [ -z "${SUMMARY}" ] || printf '%s\n\n' "${SUMMARY}"
  for n in ${ISSUES}; do printf 'Closes #%s\n' "${n}"; done
  [ -z "${ISSUES}" ] || printf '\n'
  echo "${PR}" | jq -r '[.commits[].messageBody // empty] | join("\n")' | tr -d '\r' \
    | grep -iE '^Co-Authored-By:' | awk '!seen[tolower($0)]++' || true
} > "${BODY_FILE}"

if [ "${DRY_RUN:-}" = "true" ]; then
  echo "[dry run] would squash-merge PR #${PR_NUMBER} at ${HEAD_SHA} with:"
  echo "Subject: ${SUBJECT}"
  echo "Body:"
  cat "${BODY_FILE}"
  exit 0
fi

if [ -z "${ALREADY_MERGED}" ]; then
  gh_retry gh pr merge "${PR_NUMBER}" --repo "${OWNER_REPO}" --squash --match-head-commit "${HEAD_SHA}" \
    --subject "${SUBJECT}" --body-file "${BODY_FILE}" >/dev/null \
    || refuse "gh pr merge failed for PR #${PR_NUMBER}"
fi

MERGED=$(gh_retry gh pr view "${PR_NUMBER}" --repo "${OWNER_REPO}" --json state,mergeCommit) || refuse "could not re-read PR #${PR_NUMBER}"
[ "$(echo "${MERGED}" | jq -r '.state')" = "MERGED" ] || refuse "PR #${PR_NUMBER} is not MERGED after gh pr merge"
MERGE_SHA=$(echo "${MERGED}" | jq -r '.mergeCommit.oid // ""')

# A merge into the default branch closes linked issues; one into a release branch
# is closed by close-linked-issues.yml. Wait for that, then close any left open.
CLOSED_BY_HAND=""
if [ -n "${ISSUES}" ]; then
  interval="${CLOSE_POLL_INTERVAL:-15}"
  limit="${CLOSE_POLL_LIMIT:-12}"
  view_fail_limit="${CLOSE_VIEW_FAIL_LIMIT:-3}"
  open_issues="${ISSUES}"
  i=0
  fail_rounds=0
  while [ -n "${open_issues}" ] && [ "${i}" -lt "${limit}" ]; do
    still=""
    round_failed=""
    for n in ${open_issues}; do
      if st=$(gh_retry gh issue view "${n}" --repo "${OWNER_REPO}" --json state -q .state); then
        [ "${st}" = "CLOSED" ] || still="${still} ${n}"
      else
        round_failed=1
        still="${still} ${n}"
      fi
    done
    open_issues=$(echo "${still}" | xargs)
    i=$((i + 1))
    if [ -n "${round_failed}" ]; then
      fail_rounds=$((fail_rounds + 1))
    else
      fail_rounds=0
    fi
    if [ "${fail_rounds}" -ge "${view_fail_limit}" ] && [ -n "${open_issues}" ]; then
      echo "Warning: could not read issue state from GitHub (${fail_rounds} failed polls in a row); stopped waiting." >&2
      break
    fi
    [ -z "${open_issues}" ] || [ "${i}" -ge "${limit}" ] || sleep "${interval}"
  done
  for n in ${open_issues}; do
    if gh_retry gh issue close "${n}" --repo "${OWNER_REPO}" --comment "Closed by #${PR_NUMBER} (squash-merged by verify-and-merge)." >/dev/null; then
      CLOSED_BY_HAND="${CLOSED_BY_HAND} #${n}"
    else
      echo "Warning: could not close issue #${n}; close it by hand." >&2
    fi
  done
fi

echo "Squash-merged PR #${PR_NUMBER} into ${BASE_BRANCH:-the base branch} as ${MERGE_SHA:-<unknown>}."
[ -z "${CLOSED_BY_HAND}" ] || echo "Closed by hand (the merge did not close them):${CLOSED_BY_HAND}."
if [ -d "${START_DIR}" ]; then
  echo "The worktree is still here; remove it with: conductor worktree cleanup (or conductor worktree delete)."
else
  echo "The worktree (${START_DIR}) is already removed; nothing to clean up."
fi
jq -n --arg n "${PR_NUMBER}" --arg sha "${MERGE_SHA}" '{markers: ["merged"], context: "Squash-merged PR #\($n) as \($sha[0:7])."}' \
  | { printf '<<<FLOW_OUTPUT>>>\n'; cat; printf '<<<END_FLOW_OUTPUT>>>\n'; }
